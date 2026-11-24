//! The one row a Cloud instance holds its clock, its authority and its service
//! in, and how a transaction takes it.

use super::audience_matches;
use crate::Jsonb;
use crate::database::Transaction;
use crate::store::{integer, now, unsigned};
use crate::{Error, InstanceSpec, Result};
use golia_platform_contract::Audience;

/// This barrier precedes repository and credential locks. It serializes cache
/// installation, revocation, quota accounting and authority use across workers.
/// A backwards wall clock never gives a previously accepted lease extra life.
///
/// What an instance may store is checked as a sum over all of its repositories,
/// which holds only while one transaction at a time changes what is summed: that
/// is why every transaction that writes takes the row, and takes it first.
pub(crate) fn check_instance(tx: &mut Transaction<'_>, spec: &InstanceSpec) -> Result<()> {
    let row = binding(tx)?;
    let audience: Audience = row.get::<_, Jsonb>("audience").read_serde()?;
    audience_matches(spec, &audience)?;
    let time = now(tx)?;
    if time < unsigned(row.get("clock_high_water"))? {
        return Err(Error::Precondition("Cloud authority clock moved backwards"));
    }
    if tx.is_reading() {
        return Ok(());
    }
    tx.execute(
        "UPDATE alt_cloud_binding SET clock_high_water=$1 WHERE singleton",
        &[&integer(time)?],
    )?;
    Ok(())
}
/// The binding, locked against every other transaction that is not a read. A read
/// sees it as it was when the read began and waits for nobody.
pub(super) fn binding(tx: &mut Transaction<'_>) -> Result<alt_pg::Row> {
    let row = if tx.is_reading() {
        tx.query_opt("SELECT audience,runtime_key_digest,provision_digest,clock_high_water,authority_revision,service,service_denied_at FROM alt_cloud_binding WHERE singleton", &[])?
    } else {
        tx.query_opt("SELECT audience,runtime_key_digest,provision_digest,clock_high_water,authority_revision,service,service_denied_at FROM alt_cloud_binding WHERE singleton FOR UPDATE", &[])?
    };
    row.ok_or(Error::Precondition("Cloud platform binding required"))
}
pub(super) fn check_revision(tx: &mut Transaction<'_>, revision: u64) -> Result<()> {
    let row = binding(tx)?;
    if revision < unsigned(row.get("authority_revision"))? {
        return Err(Error::Unauthorized);
    }
    // the transaction that stored these claims recorded their revision; a read
    // records nothing
    if tx.is_reading() {
        return Ok(());
    }
    tx.execute(
        "UPDATE alt_cloud_binding SET authority_revision=$1 WHERE singleton AND authority_revision<$1",
        &[&integer(revision)?],
    )?;
    Ok(())
}
