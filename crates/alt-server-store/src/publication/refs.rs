use super::{Error, PublicationWork, Result, Transaction, VerifiedGraph};
use crate::database::BATCH_ROWS;
use alt_repository_service::ValidatedRef;

/// Apply already-validated refs and their exact retention closure under the
/// caller's repository barrier and publication transaction. All arrays are
/// bounded independently of the total number of refs or reachable objects.
pub(super) fn replace(
    tx: &mut Transaction<'_>,
    work: &PublicationWork,
    graph: &VerifiedGraph,
    updates: &[ValidatedRef],
) -> Result<()> {
    let repository = &work.key.scope.repository_instance_id;
    for batch in updates.chunks(BATCH_ROWS) {
        let pins: Vec<_> = batch.iter().map(|r| format!("ref:{}", r.name)).collect();
        tx.execute(
            "DELETE FROM alt_pins WHERE repository=$1 AND pin_id=ANY($2)",
            &[repository, &pins],
        )?;
        let mut pin_ids = Vec::with_capacity(BATCH_ROWS);
        let mut digests = Vec::with_capacity(BATCH_ROWS);
        let mut names = Vec::new();
        let mut oids = Vec::new();
        let mut deleted = Vec::new();
        for (update, pin) in batch.iter().zip(&pins) {
            let Some(oid) = update.new else {
                deleted.push(update.name.as_str());
                continue;
            };
            names.push(update.name.as_str());
            oids.push(oid.to_string());
            for reachable in graph.reachable(oid)? {
                let (_, digest, _) = work
                    .source
                    .objects
                    .get(&reachable)
                    .ok_or(Error::Integrity("object outside worker source"))?;
                pin_ids.push(pin.as_str());
                digests.push(digest.as_str());
                if digests.len() == BATCH_ROWS {
                    insert_pins(tx, repository, &pin_ids, &digests)?;
                    pin_ids.clear();
                    digests.clear();
                }
            }
        }
        if !digests.is_empty() {
            insert_pins(tx, repository, &pin_ids, &digests)?;
        }
        if !names.is_empty() {
            tx.execute(
                "INSERT INTO alt_refs(repository,name,oid) SELECT $1,name,oid FROM unnest($2::text[],$3::text[]) AS refs(name,oid) ON CONFLICT(repository,name) DO UPDATE SET oid=excluded.oid",
                &[repository, &names, &oids],
            )?;
        }
        if !deleted.is_empty() {
            tx.execute(
                "DELETE FROM alt_refs WHERE repository=$1 AND name=ANY($2)",
                &[repository, &deleted],
            )?;
        }
    }
    Ok(())
}

fn insert_pins(
    tx: &mut Transaction<'_>,
    repository: &str,
    pins: &[&str],
    digests: &[&str],
) -> Result<()> {
    // Each affected ref's old pins were removed above. Equal canonical bytes
    // under different Git kinds can repeat inside or across these batches;
    // their one permanent pin is identical, including its NULL expiry.
    tx.execute(
        "INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) SELECT $1,pin,digest,NULL FROM unnest($2::text[],$3::text[]) AS members(pin,digest) ON CONFLICT(repository,pin_id,blake3) DO NOTHING",
        &[&repository, &pins, &digests],
    )?;
    Ok(())
}
