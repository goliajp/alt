//! The index a merge that stopped on conflicts left. No operation records
//! it, so it is kept as a recovery snapshot and noted beside the index
//! with the last operation of the log at that time: an index file that is
//! torn later is put back to the conflicts, not to what was there before
//! the merge.

use super::*;

/// The index a merge that stopped left, kept beside the index: no
/// operation records it.
#[derive(alt_json::ToJson, alt_json::FromJson)]
pub(super) struct Stopped {
    version: u32,
    /// the snapshot of the index
    pub(super) index: String,
    /// the last operation of the log when it was written
    pub(super) after: String,
}

fn stopped_path(index_path: &Path) -> PathBuf {
    index_path.with_file_name("index.stopped")
}

pub(super) fn stopped_at(index_path: &Path) -> Option<Stopped> {
    let bytes = std::fs::read(stopped_path(index_path)).ok()?;
    let stopped: Stopped = alt_json::from_slice(&bytes).ok()?;
    (stopped.version == 1).then_some(stopped)
}

impl NativeRepo<'_> {
    /// Keeps `index`, which a merge that stopped has just put in place,
    /// where it is found again should the index file be torn later: no
    /// operation records this index.
    pub(in crate::native) fn keep_stopped(&self, index: Index) -> Res<()> {
        let Some(after) = self.store.refs.head_op() else {
            return Ok(());
        };
        let stopped = Stopped {
            version: 1,
            index: self.snapshot(&canonical_index(index, self.store.algo()?))?,
            after: after.to_string(),
        };
        let bytes = alt_json::to_vec(&stopped)?;
        Ok(durable_write(&stopped_path(&self.index_path), &bytes)?)
    }
}
