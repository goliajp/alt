//! Aggregate Cloud reservations share the instance authority barrier. Storage
//! counts catalogued bytes per repository (including resumable chunks). Ingress
//! counts newly accepted root content once per request, in its UTC month;
//! resumable chunks do not add a second ingress charge for the same root.
//! replays of already verified content do not consume the allowance twice.
use crate::cloud::service;
use crate::database::Transaction;
use crate::store::{integer, unsigned};
use crate::{Error, InstanceSpec, Result};
use alt_remote_protocol::RequestKey;
use golia_platform_contract::AltServiceAuthorization;

fn month(tx: &mut Transaction<'_>) -> Result<String> {
    Ok(tx
        .query_one(
            "SELECT to_char(clock_timestamp() AT TIME ZONE 'UTC','YYYY-MM')",
            &[],
        )?
        .get(0))
}
fn check_storage(tx: &mut Transaction<'_>, policy: &AltServiceAuthorization) -> Result<()> {
    let row=tx.query_one("SELECT COALESCE(sum(stored_bytes::numeric + reserved_bytes::numeric + link_bytes::numeric),0) <= $1::bigint::numeric FROM alt_repositories", &[&integer(policy.limits.stored_bytes.0)?])?;
    if !row.get::<_, bool>(0) {
        return Err(Error::Limit("Cloud aggregate storage quota"));
    }
    Ok(())
}
fn reserve_month(tx: &mut Transaction<'_>, month: &str, bytes: u64, limit: u64) -> Result<()> {
    tx.execute(
        "INSERT INTO alt_cloud_ingress(month) VALUES($1) ON CONFLICT DO NOTHING",
        &[&month],
    )?;
    if tx.execute("UPDATE alt_cloud_ingress SET reserved_bytes=reserved_bytes+$2 WHERE month=$1 AND $2<=$3-accepted_bytes-reserved_bytes", &[&month,&integer(bytes)?,&integer(limit)?])?!=1 {
        return Err(Error::Limit("Cloud monthly accepted ingress quota"));
    }
    Ok(())
}
pub(crate) fn reserve(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    key: &RequestKey,
    bytes: u64,
) -> Result<()> {
    let Some(policy) = service(tx, spec)? else {
        return Ok(());
    };
    check_storage(tx, &policy)?;
    let month = month(tx)?;
    reserve_month(tx, &month, bytes, policy.limits.monthly_ingress_bytes.0)?;
    tx.execute(
        "INSERT INTO alt_cloud_reservations VALUES($1,$2,$3,$4,$5,$6)",
        &[
            &key.scope.repository_instance_id,
            &integer(key.scope.recovery_epoch)?,
            &key.actor_id,
            &key.request_id,
            &month,
            &integer(bytes)?,
        ],
    )?;
    Ok(())
}
/// Called only for a selected content item which was not yet verified for this
/// request. Settlement is atomic with the byte catalog and request progress.
pub(crate) fn accept(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    key: &RequestKey,
    bytes: u64,
) -> Result<()> {
    let Some(policy) = service(tx, spec)? else {
        return Ok(());
    };
    check_storage(tx, &policy)?;
    let row=tx.query_opt("SELECT month,remaining_bytes FROM alt_cloud_reservations WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4", &[&key.scope.repository_instance_id,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id])?.ok_or(Error::Precondition("Cloud upload reservation missing"))?;
    let previous: String = row.get(0);
    let remaining = unsigned(row.get(1))?;
    let current = month(tx)?;
    if previous != current {
        tx.execute(
            "UPDATE alt_cloud_ingress SET reserved_bytes=reserved_bytes-$2 WHERE month=$1",
            &[&previous, &integer(remaining)?],
        )?;
        reserve_month(
            tx,
            &current,
            remaining,
            policy.limits.monthly_ingress_bytes.0,
        )?;
        tx.execute("UPDATE alt_cloud_reservations SET month=$5 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4", &[&key.scope.repository_instance_id,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&current])?;
    }
    if bytes > remaining {
        return Err(Error::Integrity(
            "Cloud ingress exceeds selected reservation",
        ));
    }
    if tx.execute("UPDATE alt_cloud_ingress SET accepted_bytes=accepted_bytes+$2,reserved_bytes=reserved_bytes-$2 WHERE month=$1 AND accepted_bytes+reserved_bytes<=$3", &[&current,&integer(bytes)?,&integer(policy.limits.monthly_ingress_bytes.0)?])?!=1 {
        return Err(Error::Limit("Cloud ingress allowance reduced"));
    }
    tx.execute("UPDATE alt_cloud_reservations SET remaining_bytes=remaining_bytes-$5 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4", &[&key.scope.repository_instance_id,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&integer(bytes)?])?;
    Ok(())
}
pub(crate) fn release(tx: &mut Transaction<'_>, key: &RequestKey) -> Result<()> {
    if let Some(row)=tx.query_opt("DELETE FROM alt_cloud_reservations WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4 RETURNING month,remaining_bytes", &[&key.scope.repository_instance_id,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id])? {
        tx.execute("UPDATE alt_cloud_ingress SET reserved_bytes=reserved_bytes-$2 WHERE month=$1", &[&row.get::<_,String>(0),&row.get::<_,i64>(1)])?;
    }
    Ok(())
}
pub(crate) fn check_publication(tx: &mut Transaction<'_>, spec: &InstanceSpec) -> Result<()> {
    if let Some(policy) = service(tx, spec)? {
        check_storage(tx, &policy)?;
        let current = month(tx)?;
        if let Some(row) = tx.query_opt(
            "SELECT accepted_bytes,reserved_bytes FROM alt_cloud_ingress WHERE month=$1",
            &[&current],
        )? && unsigned(row.get(0))?.saturating_add(unsigned(row.get(1))?)
            > policy.limits.monthly_ingress_bytes.0
        {
            return Err(Error::Limit("Cloud ingress allowance reduced"));
        }
    }
    Ok(())
}

// Portable migration has its own immutable request table; it never invents a
// native publication completion solely to reuse quota accounting.
pub(crate) fn reserve_portable(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    key: &RequestKey,
    bytes: u64,
) -> Result<()> {
    let Some(policy) = service(tx, spec)? else {
        return Ok(());
    };
    check_storage(tx, &policy)?;
    let current = month(tx)?;
    reserve_month(tx, &current, bytes, policy.limits.monthly_ingress_bytes.0)?;
    tx.execute("INSERT INTO alt_cloud_portable_reservations(repository,import_id,month,remaining_bytes) VALUES($1,$2,$3,$4)", &[&key.scope.repository_instance_id,&key.request_id,&current,&integer(bytes)?])?;
    Ok(())
}
pub(crate) fn accept_portable(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    key: &RequestKey,
    bytes: u64,
) -> Result<()> {
    let Some(policy) = service(tx, spec)? else {
        return Ok(());
    };
    check_storage(tx, &policy)?;
    let row = tx.query_opt("SELECT month,remaining_bytes FROM alt_cloud_portable_reservations WHERE repository=$1 AND import_id=$2", &[&key.scope.repository_instance_id,&key.request_id])?.ok_or(Error::Precondition("Cloud portable reservation missing"))?;
    let previous: String = row.get(0);
    let remaining = unsigned(row.get(1))?;
    if bytes > remaining {
        return Err(Error::Integrity(
            "Cloud portable ingress exceeds reservation",
        ));
    }
    let current = month(tx)?;
    if previous != current {
        tx.execute(
            "UPDATE alt_cloud_ingress SET reserved_bytes=reserved_bytes-$2 WHERE month=$1",
            &[&previous, &integer(remaining)?],
        )?;
        reserve_month(
            tx,
            &current,
            remaining,
            policy.limits.monthly_ingress_bytes.0,
        )?;
        tx.execute("UPDATE alt_cloud_portable_reservations SET month=$3 WHERE repository=$1 AND import_id=$2", &[&key.scope.repository_instance_id,&key.request_id,&current])?;
    }
    if tx.execute("UPDATE alt_cloud_ingress SET accepted_bytes=accepted_bytes+$2,reserved_bytes=reserved_bytes-$2 WHERE month=$1 AND accepted_bytes+reserved_bytes<=$3", &[&current,&integer(bytes)?,&integer(policy.limits.monthly_ingress_bytes.0)?])? != 1 {
        return Err(Error::Limit("Cloud portable ingress allowance reduced"));
    }
    tx.execute("UPDATE alt_cloud_portable_reservations SET remaining_bytes=remaining_bytes-$3 WHERE repository=$1 AND import_id=$2", &[&key.scope.repository_instance_id,&key.request_id,&integer(bytes)?])?;
    Ok(())
}
pub(crate) fn release_portable(tx: &mut Transaction<'_>, key: &RequestKey) -> Result<()> {
    if let Some(row) = tx.query_opt("DELETE FROM alt_cloud_portable_reservations WHERE repository=$1 AND import_id=$2 RETURNING month,remaining_bytes", &[&key.scope.repository_instance_id,&key.request_id])? {
        tx.execute("UPDATE alt_cloud_ingress SET reserved_bytes=reserved_bytes-$2 WHERE month=$1", &[&row.get::<_,String>(0),&row.get::<_,i64>(1)])?;
    }
    Ok(())
}

/// New signed event/receipt metadata only. Replays have zero delta. Delivery
/// receipt capacity is reserved in repository storage; a reduced/expired Cloud
/// allowance leaves the receipt pending rather than inventing an acceptance.
pub(crate) fn accept_link_metadata(
    tx: &mut Transaction<'_>,
    spec: &InstanceSpec,
    bytes: u64,
) -> Result<()> {
    let Some(policy) = service(tx, spec)? else {
        return Ok(());
    };
    check_storage(tx, &policy)?;
    if bytes == 0 {
        return Ok(());
    }
    let current = month(tx)?;
    tx.execute(
        "INSERT INTO alt_cloud_ingress(month) VALUES($1) ON CONFLICT DO NOTHING",
        &[&current],
    )?;
    if tx.execute("UPDATE alt_cloud_ingress SET accepted_bytes=accepted_bytes+$2 WHERE month=$1 AND accepted_bytes::numeric+reserved_bytes::numeric+$2::bigint::numeric<=$3::bigint::numeric", &[&current,&integer(bytes)?,&integer(policy.limits.monthly_ingress_bytes.0)?])?!=1 {
        return Err(Error::Limit("Cloud monthly accepted ingress quota"));
    }
    Ok(())
}
