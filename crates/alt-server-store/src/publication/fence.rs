//! The checks every transition of a leased request repeats, and its terminal step.
use super::*;

pub(crate) fn worker(tx: &mut Transaction<'_>, work: &PublicationWork) -> Result<postgres::Row> {
    let row = request_row(tx, &work.key)?.ok_or(Error::Precondition("request not registered"))?;
    let time = now(tx)?;
    active(&row, time)?;
    if unsigned(row.get("fence"))? != work.fence || unsigned(row.get("worker_lease_until"))? <= time
    {
        return Err(Error::Precondition("stale publication worker"));
    }
    Ok(row)
}
pub(crate) fn check_graph_pins(
    tx: &mut Transaction<'_>,
    work: &PublicationWork,
    graph: &VerifiedGraph,
) -> Result<()> {
    let repository = &work.key.scope.repository_instance_id;
    let pin_id = upload_pin(&work.key);
    let time = now(tx)?;
    let objects: Vec<_> = graph.objects().collect();
    for batch in objects.chunks(crate::database::BATCH_ROWS) {
        // objects held by a segment are protected by the repository's closure, not by pins
        let mut selected = Vec::new();
        for oid in batch {
            match work.source.objects.get(oid) {
                Some(found) => selected.push(found),
                None if <[u8; 20]>::try_from(oid.as_bytes())
                    .is_ok_and(|key| work.source.segments.locate_any(&key)) => {}
                None => return Err(Error::Integrity("graph outside authorized source")),
            }
        }
        let digests: Vec<_> = selected
            .iter()
            .map(|(_, digest, _)| digest.as_str())
            .collect();
        // Keep each lookup bounded independently. A join planned against a
        // freshly populated catalog can repeatedly scan this request's entire
        // pin set for each digest. Both observations use the same repository
        // lock and transaction as the caller's current publication checks.
        let present: BTreeMap<String, i64> = tx.query("SELECT blake3,bytes FROM alt_blobs WHERE repository=$1 AND blake3=ANY($2) AND state='stored'", &[repository,&digests])?
            .into_iter().map(|row| (row.get(0),row.get(1))).collect();
        let pinned: BTreeSet<String> = tx.query("SELECT blake3 FROM alt_pins WHERE repository=$1 AND pin_id=$2 AND blake3=ANY($3) AND expires_at>$4", &[repository,&pin_id,&digests,&integer(time)?])?
            .into_iter().map(|row| row.get(0)).collect();
        for (_, digest, bytes) in selected {
            let actual = present
                .get(digest)
                .filter(|_| pinned.contains(digest))
                .ok_or(Error::Precondition("object pin expired or fenced"))?;
            if unsigned(*actual)? != *bytes {
                return Err(Error::Integrity("pinned object size"));
            }
        }
    }
    Ok(())
}
pub(crate) fn finish(
    tx: &mut Transaction<'_>,
    key: &RequestKey,
    state: &str,
    result: &(dyn postgres::types::ToSql + Sync),
    reservation: u64,
) -> Result<()> {
    let repository = &key.scope.repository_instance_id;
    crate::cloud_quota::release(tx, key)?;
    tx.execute("UPDATE alt_requests SET state=$5,result=$6,reserved_bytes=0,worker_lease_until=0 WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4",&[repository,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id,&state,result])?;
    tx.execute(
        "UPDATE alt_repositories SET reserved_bytes=reserved_bytes-$2 WHERE id=$1",
        &[repository, &integer(reservation)?],
    )?;
    tx.execute("DELETE FROM alt_request_content WHERE repository=$1 AND epoch=$2 AND actor_id=$3 AND request_id=$4", &[repository,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id])?;
    tx.execute(
        "DELETE FROM alt_pins WHERE repository=$1 AND pin_id=$2",
        &[repository, &upload_pin(key)],
    )?;
    Ok(())
}
