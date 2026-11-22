//! Where the snapshot is anchored, and when it is written anew.
//!
//! The snapshot is derived from the log, and only from what the log's
//! marker records as synced: its anchor never lies past that boundary.
//! Past it a record may turn out not to read after a power cut, and the
//! log then ends there for every command; a snapshot anchored further on
//! would tell of ops that are gone, and an open that began after it
//! would not see the record that does not read. The ops past the
//! boundary are folded in from the log by every open, which has to look
//! at each of them anyway to tell where the log ends.

use std::path::Path;

use crate::snapshot::{self, Snapshot};
use crate::{RefError, RefStore, tx};

/// Write a snapshot every this many ops (replay-cost ceiling).
const SNAPSHOT_EVERY: usize = 1024;
/// Also write one once this many log bytes follow the last snapshot — or
/// half the snapshot's own size, when that is more: opening hashes every
/// byte after the snapshot, and reads the snapshot itself anyway.
const SNAPSHOT_BYTES: u64 = 16 << 10;

/// The snapshot at `path` for an open to begin at, and what the marker
/// of the log in `log_dir` told of its boundary where it was read: it
/// is where the snapshot's note does not cover the anchor. A snapshot
/// anchored past the boundary, which another build may have written, is
/// not begun at: the open reads the log from its first record.
pub(crate) fn read(path: &Path, log_dir: &Path) -> (Option<Snapshot>, Option<Option<u64>>) {
    let Some(snap) = snapshot::read(path) else {
        return (None, None);
    };
    let end = snap.anchor.map_or(0, |anchor| anchor.end);
    if snap.synced.is_some_and(|noted| end <= noted) {
        return (Some(snap), None);
    }
    let told = alt_oplog::bounded_in(log_dir);
    let within = told.is_none_or(|whole| end <= whole);
    (within.then_some(snap), Some(told))
}

impl RefStore {
    /// Whether enough of the log that is known synced follows the
    /// snapshot for it to be written anew.
    pub(crate) fn snapshot_is_due(&self) -> bool {
        let synced = self.synced.max(self.oplog.synced_here());
        let behind = (self.oplog.appended_len().min(synced)).saturating_sub(self.snapshot_end);
        behind > 0
            && self.ops_since_snapshot > 0
            && (self.ops_since_snapshot >= SNAPSHOT_EVERY
                || behind >= SNAPSHOT_BYTES.max(self.snapshot_len / 2))
    }

    pub(crate) fn note_op(&mut self) -> Result<(), RefError> {
        self.ops_since_snapshot += 1;
        if self.snapshot_is_due() {
            // the operation is in the log already; the snapshot is a cache
            let _ = self.snapshot();
        }
        Ok(())
    }

    /// Writes the snapshot now (also happens automatically every
    /// [`SNAPSHOT_EVERY`] ops), anchored at the last op the log's marker
    /// records as synced: the head, where the whole log is.
    pub fn snapshot(&mut self) -> Result<(), RefError> {
        // every op the log holds must be in `refs` first: another writer's
        // ops are folded in under the append lock, not here
        if self.applied != self.oplog.recent().len() {
            return Ok(());
        }
        // ops this handle appended were synced since it was opened; a log
        // whose marker records no boundary is taken for synced
        let mut within = u64::MAX;
        if !self.known_synced()
            && let Some(whole) = self.oplog.bounded_to()
        {
            (self.synced, within) = (whole, whole);
        }
        let Some((at, after)) = self.oplog.anchor_within(within) else {
            return Ok(()); // nothing to anchor a snapshot to
        };
        if at.end < self.snapshot_end {
            return Ok(());
        }
        if let Some(dir) = self.snapshot_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // the refs as they were at that op: what the ops after it changed
        // is taken back, and made again once the snapshot is written
        let recent = self.oplog.recent();
        let mut later = Vec::new();
        for op in &recent[recent.len() - after..] {
            later.extend(tx::parse_tx(&op.payload)?);
        }
        for tx in later.iter().rev() {
            self.refs.turn(&tx.changes, true);
        }
        let (refs, synced) = (&self.refs, self.synced);
        let written = snapshot::write(&self.snapshot_path, &at, synced, refs.len(), refs.iter());
        for tx in &later {
            self.refs.turn(&tx.changes, false);
        }
        self.snapshot_len = written?;
        self.snapshot_end = at.end;
        self.ops_since_snapshot = after;
        Ok(())
    }
}
