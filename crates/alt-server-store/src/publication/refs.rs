use super::{Result, Transaction};
use crate::database::BATCH_ROWS;
use alt_repository_service::ValidatedRef;

/// Apply already-validated refs under the caller's repository barrier and
/// publication transaction. What they reach is held by the pack segments; no
/// retention is recorded here.
pub(crate) fn replace(
    tx: &mut Transaction<'_>,
    repository: &str,
    updates: &[ValidatedRef],
) -> Result<()> {
    for batch in updates.chunks(BATCH_ROWS) {
        let mut created = (Vec::new(), Vec::new());
        let mut moved = (Vec::new(), Vec::new());
        let mut deleted = Vec::new();
        for update in batch {
            match (update.expected, update.new) {
                (None, Some(oid)) => {
                    created.0.push(update.name.as_str());
                    created.1.push(oid.to_string());
                }
                (Some(_), Some(oid)) => {
                    moved.0.push(update.name.as_str());
                    moved.1.push(oid.to_string());
                }
                (_, None) => deleted.push(update.name.as_str()),
            }
        }
        // validation saw each name absent or present under the repository lock, so a
        // new row cannot collide and needs no conflict arbitration
        if !created.0.is_empty() {
            tx.execute(
                "INSERT INTO alt_refs(repository,name,oid) SELECT $1,name,oid FROM unnest($2::text[],$3::text[]) AS refs(name,oid)",
                &[&repository, &created.0, &created.1],
            )?;
        }
        if !moved.0.is_empty() {
            tx.execute(
                "INSERT INTO alt_refs(repository,name,oid) SELECT $1,name,oid FROM unnest($2::text[],$3::text[]) AS refs(name,oid) ON CONFLICT(repository,name) DO UPDATE SET oid=excluded.oid",
                &[&repository, &moved.0, &moved.1],
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
