//! Building index runs over the append files.
//!
//! A run is a cache that lookups take at its word where it holds no
//! entry, and a record behind one is not read again by whoever opens the
//! store. So a run is built over records that were synced and over no
//! others: a writer that syncs builds them once it has fsynced, an opener
//! builds them up to the boundaries the marker records, and a writer
//! that leaves its appends unsynced builds none. What such a writer
//! appended stays in the tail every opener reads, where a record a power
//! cut left without its content is found.

use std::fs::File;
use std::path::Path;

use alt_store::{Basis, StoreError};

use super::Odb;
use super::open::Files;
use crate::OdbError;
use crate::lock::{release_exclusive, try_hold_exclusive};
use crate::synced::{Log, Marker};

/// How far `log` (pack `seq`, `len` bytes long, for the pack) was synced
/// by what `marker` says; `None` where it says nothing of it. Appends
/// left unsynced lie past the boundaries of a marker that tells of them.
fn synced(marker: &Marker, log: Log, seq: u32, len: u64) -> Option<u64> {
    match marker.basis(log, seq, len, || true) {
        Basis::Boundary(upto) => Some(upto),
        _ => None,
    }
}

/// Whether enough synced records of `files` have no index run for
/// [`index_synced`] to build one. The marker is read only where enough
/// records of any kind have none.
pub(super) fn wanted(files: &Files, durable: &Path) -> bool {
    let (blobs, _, map, _) = files;
    if !blobs.index_pending() && !map.index_pending(u64::MAX) {
        return false;
    }
    let marker = Marker::read(durable);
    let upto = |log| synced(&marker, log, 0, 0).unwrap_or(0);
    blobs.synced_pending(
        |seq, len| synced(&marker, Log::Pack, seq, len),
        upto(Log::Blobmap),
    ) || map.index_pending(upto(Log::Map))
}

/// A store whose synced records have outgrown their index runs — an
/// older alt wrote it, the runs were removed, a writer could not write
/// them, or the records were synced after they were appended — gets runs
/// from whoever opens it next, unless a writer is at work. The opener
/// takes the write lock for it, reads the files again under it (`read`)
/// and gives those back; `None` when a writer has the lock. That is all
/// the lock is taken for: nothing is cut, and neither the marker nor the
/// lock file is written.
pub(super) fn index_synced(
    lock: &File,
    durable: &Path,
    read: impl FnOnce() -> Result<Files, OdbError>,
) -> Result<Option<Files>, OdbError> {
    if !try_hold_exclusive(lock)? {
        return Ok(None);
    }
    let indexed = read().and_then(|mut files| {
        let marker = Marker::read(durable);
        let upto = |log| synced(&marker, log, 0, 0).unwrap_or(0);
        let (blobs, tier1, map, _) = &mut files;
        let stores = blobs.index_synced(
            |seq, len| synced(&marker, Log::Pack, seq, len),
            upto(Log::Blobmap),
        );
        let stores = stores.and_then(|()| match tier1.get_mut() {
            Some(tier1) => tier1.index_tail(upto(Log::Tier1)),
            None => Ok(()),
        });
        let indexed = stores
            .map_err(OdbError::from)
            .and_then(|()| map.index_tail(upto(Log::Map)));
        match indexed {
            // the runs are a cache: where they cannot be written, reads go on
            Ok(()) | Err(OdbError::Store(StoreError::IndexWrite(_))) => Ok(files),
            Err(damage) => Err(damage),
        }
    });
    release_exclusive(lock);
    indexed.map(Some)
}

impl Odb {
    /// Folds long enough log tails into the index runs, for a writer
    /// that syncs: the records are fsynced first. The runs are a cache:
    /// failing to write one is not an error, anything else is.
    pub(super) fn index_tails(&mut self) -> Result<(), OdbError> {
        let relaxed = alt_store::relaxed_durability();
        if relaxed && !alt_os::durability::never_synced() {
            return Ok(());
        }
        let pending = self.blobs.index_pending()
            || (self.tier1.get()).is_some_and(|tier1| tier1.index_pending(u64::MAX))
            || self.map.index_pending(u64::MAX);
        if !self.held || !pending {
            return Ok(());
        }
        // indexing the blob store opens its map: opened here, it is
        // fsynced with the rest before a run is built over it
        self.blobs.open_map()?;
        if !relaxed {
            self.blobs.fsync()?;
            if let Some(tier1) = self.tier1.get_mut() {
                tier1.sync()?;
            }
            self.map.fsync()?;
        }
        let indexed = (|| -> Result<(), StoreError> {
            self.blobs.index_tail()?;
            if let Some(tier1) = self.tier1.get_mut() {
                tier1.index_tail(u64::MAX)?;
            }
            Ok(())
        })();
        match indexed
            .map_err(OdbError::from)
            .and_then(|()| self.map.index_tail(u64::MAX))
        {
            Err(OdbError::Store(StoreError::IndexWrite(_))) => Ok(()),
            other => other,
        }
    }
}
