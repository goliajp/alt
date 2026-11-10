//! Portable signed facts, never current connector state or grants. Callers
//! authorize and hold the repository snapshot lock before accessing this table.
use crate::database::Transaction;
use crate::store::{integer, unsigned};
use crate::{Error, Result};
use golia_change_link::HistoricalEvent;

fn decode(raw: &[u8], digest: &str) -> Result<HistoricalEvent> {
    let history = HistoricalEvent::from_bytes(raw)
        .map_err(|_| Error::Integrity("change-link historical signature"))?;
    if serde_json::to_vec(&history)? != raw
        || history
            .statement
            .digest()
            .map_err(|_| Error::Integrity("change-link identity"))?
            != digest
    {
        return Err(Error::Integrity("change-link historical encoding"));
    }
    Ok(history)
}

pub(crate) fn list(
    tx: &mut Transaction<'_>,
    repository: &str,
    entries: usize,
    metadata_bytes: usize,
) -> Result<Vec<HistoricalEvent>> {
    let size = tx.query_one(
        "SELECT count(*),COALESCE(sum(octet_length(history)),0) FROM alt_link_history WHERE repository=$1",
        &[&repository],
    )?;
    if unsigned(size.get(0))? > entries as u64 || unsigned(size.get(1))? > metadata_bytes as u64 {
        return Err(Error::Limit("change-link export history"));
    }
    tx.query(
        "SELECT event_digest,history FROM alt_link_history WHERE repository=$1 ORDER BY event_digest",
        &[&repository],
    )?.into_iter().map(|row| decode(&row.get::<_, Vec<u8>>(1), &row.get::<_, String>(0))).collect()
}

/// Activation only: the empty destination has reserved these exact metadata
/// bytes in addition to all selected blobs. The caller releases its reservation
/// in this same transaction. No operational queue, claim or connector is made.
pub(crate) fn import(
    tx: &mut Transaction<'_>,
    repository: &str,
    catalog: &alt_portable::Catalog,
    reserved: u64,
) -> Result<()> {
    let bytes = catalog.change_link_bytes()?;
    if bytes > reserved {
        return Err(Error::Integrity("change-link import reservation"));
    }
    if tx.execute(
        "UPDATE alt_repositories SET link_bytes=link_bytes+$2 WHERE id=$1 AND stored_bytes::numeric+reserved_bytes::numeric+link_bytes::numeric<=quota_bytes::numeric",
        &[&repository, &integer(bytes)?],
    )? != 1 {
        return Err(Error::Limit("repository quota"));
    }
    for history in &catalog.change_links {
        let raw = serde_json::to_vec(history)?;
        let digest = history
            .statement
            .digest()
            .map_err(|_| Error::Integrity("change-link identity"))?;
        decode(&raw, &digest)?;
        tx.execute(
            "INSERT INTO alt_link_history(repository,event_digest,history) VALUES($1,$2,$3)",
            &[&repository, &digest, &raw],
        )?;
    }
    Ok(())
}

pub(crate) fn get(
    tx: &mut Transaction<'_>,
    repository: &str,
    digest: &str,
) -> Result<HistoricalEvent> {
    let row = tx
        .query_opt(
            "SELECT history FROM alt_link_history WHERE repository=$1 AND event_digest=$2",
            &[&repository, &digest],
        )?
        .ok_or(Error::Precondition("change-link event missing"))?;
    decode(&row.get::<_, Vec<u8>>(0), digest)
}

/// Append a statement or its first acknowledgement. Never replace signed
/// facts. The caller holds the repository lock and rechecks current authority.
/// Receipt storage reserved by an outbox row is released in this transaction.
pub(crate) fn retain(
    tx: &mut Transaction<'_>,
    spec: &crate::InstanceSpec,
    repository: &str,
    history: &HistoricalEvent,
    release: u64,
    reserve: u64,
) -> Result<()> {
    let digest = history
        .statement
        .digest()
        .map_err(|_| Error::Integrity("change-link identity"))?;
    let raw = serde_json::to_vec(history)?;
    decode(&raw, &digest)?;
    let old = tx.query_opt(
        "SELECT history FROM alt_link_history WHERE repository=$1 AND event_digest=$2",
        &[&repository, &digest],
    )?;
    let previous = if let Some(row) = old {
        let bytes: Vec<u8> = row.get(0);
        let value = decode(&bytes, &digest)?;
        if value.statement != history.statement
            || value.source_public_key != history.source_public_key
            || (value.acknowledgement.is_some() && value.acknowledgement != history.acknowledgement)
        {
            return Err(Error::Precondition("change-link immutable history"));
        }
        bytes.len()
    } else {
        0
    };
    let delta = raw
        .len()
        .checked_sub(previous)
        .ok_or(Error::Integrity("change-link history shrank"))? as u64;
    if tx.execute("UPDATE alt_repositories SET link_bytes=link_bytes+$2,reserved_bytes=reserved_bytes-$3+$4,transaction_sequence=transaction_sequence+1 WHERE id=$1 AND reserved_bytes>=$3 AND stored_bytes::numeric+link_bytes::numeric+reserved_bytes::numeric+$2::bigint::numeric-$3::bigint::numeric+$4::bigint::numeric<=quota_bytes::numeric", &[&repository,&integer(delta)?,&integer(release)?,&integer(reserve)?])?!=1 {
        return Err(Error::Limit("repository quota"));
    }
    crate::cloud_quota::accept_link_metadata(tx, spec, delta)?;
    tx.execute("INSERT INTO alt_link_history(repository,event_digest,history) VALUES($1,$2,$3) ON CONFLICT(repository,event_digest) DO UPDATE SET history=excluded.history", &[&repository,&digest,&raw])?;
    Ok(())
}
