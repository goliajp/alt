//! The write lock of a batch, the boundaries a writer gives the store
//! before it appends, and what it makes of appends that were left
//! unsynced.

use super::Odb;
use crate::lock::{hold_exclusive, release_exclusive};
use crate::synced::{self, Marker};
use crate::{OdbError, head, unsynced};

impl Odb {
    /// Acquires the write lock for this batch (if not already held) and brings
    /// the in-memory state up to date with concurrent writers.
    pub(super) fn acquire(&mut self) -> Result<(), OdbError> {
        if self.held {
            return Ok(());
        }
        hold_exclusive(&self.lock)?;
        self.held = true;
        let ready = self.catch_up().and_then(|()| self.bound());
        if ready.is_err() {
            // nothing was appended; the next call looks at the store again
            release_exclusive(&self.lock);
            self.held = false;
            self.blobs.end_batch();
        }
        ready
    }

    /// Releases the write lock (best-effort; the fd closing also releases it).
    pub(super) fn release(&mut self) {
        if self.held {
            self.note_checked();
            self.save_head();
            release_exclusive(&self.lock);
            self.held = false;
            self.blobs.end_batch();
        }
    }

    /// What the marker says, and whether the records past its boundaries
    /// are known to be as they were written ([`unsynced::cached`]), by
    /// the note of unsynced appends this handle last read.
    pub(crate) fn marker(&self) -> (Marker, bool) {
        let marker = Marker::read(&self.durable_path);
        let cached = unsynced::cached(&marker, *self.left.lock().unwrap());
        (marker, cached)
    }

    /// With the write lock held and every file this handle has open read
    /// to its end: a store whose marker does not record boundaries that
    /// hold — an older alt wrote it, the marker was removed, or appends
    /// were left unsynced — gets them before anything is appended. Where
    /// fsyncs are skipped the marker is made to say so instead, and the
    /// lock file to note the boot.
    ///
    /// Appends left unsynced in a boot before this one are looked at
    /// first ([`Self::settle`]): what is kept of them is what the disk
    /// holds.
    pub(super) fn bound(&mut self) -> Result<(), OdbError> {
        let marker = Marker::read(&self.durable_path);
        let note = unsynced::read(&self.lock);
        *self.left.lock().unwrap() = note;
        let cached = unsynced::cached(&marker, note);
        // a pack noted as synced is not, once the marker stops saying so
        let unseen = unsynced::unseen(&marker, note);
        if (!marker.trusted() || unseen.is_some()) && self.blobs.distrust_synced()? {
            head::forget_synced(&self.lock)?;
        }
        let looked_by = (!cached).then(|| unseen.as_ref().unwrap_or(&marker));
        self.settle(&marker, looked_by)?;
        let relaxed = alt_store::relaxed_durability();
        let behind = !marker.trusted() && !(relaxed && cached);
        if behind {
            self.blobs.fsync()?;
            self.tier1_mut()?.sync()?;
            self.map.fsync()?;
            let whole = self.appended_whole()?;
            synced::establish(&self.durable_path, whole)?;
            if !relaxed {
                head::note_synced(&self.lock, whole);
            }
        }
        if behind || unseen.is_some() {
            unsynced::forget(&self.lock)?;
            *self.left.lock().unwrap() = None;
        }
        if relaxed && (behind || marker.trusted()) {
            let whole = Marker::read(&self.durable_path)
                .recorded()
                .map_or(synced::Lens::NONE, |state| state.whole);
            let boot = unsynced::note(&self.lock, whole)?;
            *self.left.lock().unwrap() = boot.map(|boot| unsynced::Left { boot, whole });
            synced::distrust(&self.durable_path)?;
        }
        self.head.bounded = true;
        Ok(())
    }
}
