use super::{Result, Transaction};
use crate::database::BATCH_ROWS;
use alt_repository_service::ValidatedRef;

/// Apply already-validated refs under the caller's repository barrier and
/// publication transaction. What they reach is held by the pack segments; no
/// retention is recorded here.
pub(super) fn replace(
    tx: &mut Transaction<'_>,
    repository: &str,
    updates: &[ValidatedRef],
) -> Result<()> {
    for batch in updates.chunks(BATCH_ROWS) {
        let mut names = Vec::new();
        let mut oids = Vec::new();
        let mut deleted = Vec::new();
        for update in batch {
            match update.new {
                Some(oid) => {
                    names.push(update.name.as_str());
                    oids.push(oid.to_string());
                }
                None => deleted.push(update.name.as_str()),
            }
        }
        if !names.is_empty() {
            tx.execute(
                "INSERT INTO alt_refs(repository,name,oid) SELECT $1,name,oid FROM unnest($2::text[],$3::text[]) AS refs(name,oid) ON CONFLICT(repository,name) DO UPDATE SET oid=excluded.oid",
                &[&repository, &names, &oids],
            )?;
        }
        if !deleted.is_empty() {
            tx.execute(
                "DELETE FROM alt_refs WHERE repository=$1 AND name=ANY($2)",
                &[&repository, &deleted],
            )?;
        }
    }
    Ok(())
}
