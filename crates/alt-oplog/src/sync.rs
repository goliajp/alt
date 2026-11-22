//! Making what a handle appended durable, and recording how far.

use crate::left::open_sync_lock;
use crate::lock::{lock_exclusive, unlock};
use crate::sink::OpLogSink;
use crate::synced::{self, Marker, SyncNamed};
use crate::{OpLog, OpLogError};

impl OpLog {
    /// Durability point for everything appended so far. Group commit: fsync
    /// only if another committer has not already made our appends durable, so
    /// N concurrent commits coalesce to ~1 fsync. With `ALT_RELAXED_DURABILITY`
    /// the fsync is skipped entirely (appended bytes stay cross-process visible
    /// regardless).
    pub fn sync(&mut self) -> Result<(), OpLogError> {
        if relaxed_durability() {
            return Ok(());
        }
        if self.defer {
            // the daemon's group-commit coordinator fsyncs off the write path
            // (via `sync_handle`) and then records how far this append reached
            self.write_count += 1;
            self.defer_sync();
            return Ok(());
        }
        self.sync_to(self.len_bytes)
    }

    /// Turns deferred durability on or off (the daemon turns it on so its
    /// group-commit coordinator batches fsyncs in-process; the direct CLI
    /// leaves it off and fsyncs inline).
    pub fn set_defer_durability(&mut self, on: bool) {
        self.defer = on;
    }

    /// A monotonic count of deferred writes — the daemon snapshots it around a
    /// request to tell whether the command wrote.
    pub fn write_count(&self) -> u64 {
        self.write_count
    }

    /// An independent fsync handle on the (stable, append-only) `log` file,
    /// for the daemon's off-write-path fsync.
    pub fn sync_handle(&self) -> Result<OpLogSink, OpLogError> {
        Ok(OpLogSink {
            file: self.file.try_clone()?,
            appended: self.deferred.clone(),
            durable_path: self.durable_path.clone(),
            named: self.named.clone(),
            note: self.note.clone(),
        })
    }

    /// Has `sync` called before this log, or a sink made from it
    /// afterwards, records as synced ops that other writers appended and
    /// nobody has recorded yet. What such an op names its writer stored
    /// before appending it but may not have synced: `sync` syncs it, so
    /// that no op is recorded as synced ahead of what it names. It is
    /// called with no lock of the log held. Where a boundary is about to
    /// be set over ops that were appended while appends were left
    /// unsynced, `sync` is given those ops: whatever else their writers
    /// left unsynced is to be synced as well. Otherwise it is given none.
    pub fn sync_named_with(&mut self, sync: SyncNamed) {
        self.named = Some(sync);
    }

    /// Has this log, and a sink made from it afterwards, note at `place`
    /// how far the marker says the log is synced whenever they raise it.
    pub fn note_synced_at(&mut self, place: crate::NotePlace) {
        self.note = Some(place);
    }

    /// The boundary the marker records: up to it the log was synced,
    /// whether or not appends were left unsynced past it. `None` where
    /// it records none. Reads the marker and takes no lock.
    pub fn bounded_to(&self) -> Option<u64> {
        bounded_to(&self.durable_path)
    }

    /// The fsync-coalescing core: fsync only if no other committer already made
    /// our target durable (the inline, direct-CLI path), and record `target`
    /// (a record boundary) as synced either way.
    fn sync_to(&mut self, target: u64) -> Result<(), OpLogError> {
        let marker = Marker::read(&self.durable_path);
        if marker.generation() == self.generation && marker.records(target) {
            self.note_synced(target);
            return Ok(());
        }
        synced::sync_named(&self.named, &marker, self.others_to)?;
        let sync_lock = open_sync_lock(&self.sync_lock_path)?;
        lock_exclusive(&sync_lock)?;
        let result = (|| -> Result<(), OpLogError> {
            let marker = Marker::read(&self.durable_path);
            if marker.generation() != self.generation {
                // records were cut since the append: not this one's, or
                // there is nothing of it to sync
                let size = self.file.metadata()?.len();
                if !self.holds_head(size)? {
                    return Err(OpLogError::CutBack);
                }
                self.generation = marker.generation();
            }
            if marker.records(target) {
                self.note_synced(target);
                return Ok(());
            }
            // a cut of the log waits for the sync lock: the length measured
            // here is recorded before any cut brings it down
            let eof = match marker.recorded() {
                Some(state) if state.eof >= target => None,
                _ => {
                    let eof = self.file.metadata()?.len();
                    self.file.sync_all()?;
                    self.unsynced = false;
                    Some(eof)
                }
            };
            let whole = synced::record(&self.durable_path, self.generation, target, eof)?;
            if let Some(whole) = whole {
                self.note_synced(whole);
                if let Some(note) = &self.note {
                    note.raise(whole);
                }
            }
            Ok(())
        })();
        let _ = unlock(&sync_lock);
        result
    }
}

/// [`OpLog::bounded_to`] of the log in `dir`, for whoever has not opened
/// it yet.
pub fn bounded_in(dir: &std::path::Path) -> Option<u64> {
    bounded_to(&dir.join("durable"))
}

fn bounded_to(durable: &std::path::Path) -> Option<u64> {
    Marker::read(durable).judged_from().map(|(whole, _)| whole)
}

/// Whether the running thread leaves what it writes unsynced: a command
/// that only stages does ([`alt_os::durability`]).
pub fn relaxed_durability() -> bool {
    alt_os::durability::unsynced()
}
