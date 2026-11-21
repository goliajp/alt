//! What a prepared index change names in the object store.
//!
//! The blobs of an index change are written before its intent is
//! appended to the operation log, and both are synced before the index
//! is replaced. After a power cut the kernel may have written the intent
//! out and not the blobs: such an intent lies past the boundary the
//! log's marker records, like a ref transaction whose objects never
//! arrived, and is cut and passed over by the same rule.

use std::sync::Arc;

use super::*;

/// The recovery snapshot `id` of the store under `alt_dir`.
pub(super) fn read_snapshot_at(alt_dir: &Path, id: &str) -> Res<Vec<u8>> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid recovery snapshot identity".into());
    }
    let bytes = std::fs::read(alt_dir.join("recovery/objects").join(id))?;
    if hash(&bytes) != id {
        return Err("corrupt recovery snapshot".into());
    }
    Ok(bytes)
}

/// The blobs of the index that `op` prepares, where it is the intent of
/// an index change.
fn blobs(alt_dir: &Path, op: &alt_oplog::Op) -> Res<Vec<ObjectId>> {
    if op.payload.first() != Some(&PREPARE) {
        return Ok(Vec::new());
    }
    let intent: IndexIntent = decode(&op.payload, PREPARE)?;
    let index = read_snapshot_at(alt_dir, &intent.after)?;
    let algo = alt_repo::native_object_format(alt_dir)?;
    let entries = Index::parse(&index, algo)?.entries.into_iter();
    Ok(entries
        .filter(|entry| entry.mode & 0o170000 != 0o160000)
        .map(|entry| entry.oid)
        .collect())
}

/// For whoever looks for ops that were never synced and name what is not
/// stored: the blobs an index change of the store under `alt_dir` names.
pub fn prepared_blobs(alt_dir: &Path) -> alt_refs::AlsoNamed {
    let alt_dir = alt_dir.to_owned();
    // an intent or a snapshot that does not read names nothing here: the
    // recovery of that operation reports it
    Arc::new(move |op| blobs(&alt_dir, op).unwrap_or_default())
}

/// Writes the store under `alt_dir` out as a git repository in `target`,
/// without what was never synced and names what is not stored, an add
/// among it; a ref that reading past damage leaves out is told on stderr.
pub fn export_git(
    alt_dir: &Path,
    target: &Path,
) -> Result<alt_export::ExportReport, alt_export::ExportError> {
    let report = alt_export::export_git_naming(alt_dir, target, Some(&prepared_blobs(alt_dir)))?;
    for (name, oid) in &report.left_out {
        eprintln!("warning: left out ref {name}: it points at {oid}, which is not stored");
    }
    Ok(report)
}
