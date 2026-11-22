//! The write lock of a batch, the boundaries a writer gives the store
//! before it appends, and what it makes of appends that were left
//! unsynced.

use super::Odb;
use super::open::Access;
use crate::lock::{hold_exclusive, release_exclusive};
use crate::synced::{self, Marker};
use crate::unsynced::Left;
use crate::{OdbError, default_registry, head, repair, unsynced};

impl Odb {
    /// Acquires the write lock for this batch (if not already held) and brings
    /// the in-memory state up to date with concurrent writers. Where
    /// records were taken out of the store since this handle read it,
    /// the handle reads all of the store again: what was appended after
    /// a cut can make a file as long as it was.
    pub(super) fn acquire(&mut self) -> Result<(), OdbError> {
        let mut again = 0;
        while !self.held {
            hold_exclusive(&self.lock)?;
            self.held = true;
            if !alt_store::relaxed_durability() {
                // a writer that syncs records all it finds as synced: what
                // the running boot left unsynced is read as what another left
                *self.left.lock().unwrap() = None;
            }
            let (note, notes) = unsynced::read(&self.lock);
            self.noted = notes.generation;
            let ready = self.caught_up(note, notes);
            let ready = ready.map_err(|error| repair::told(&self.alt_dir, error));
            if !matches!(ready, Ok(true)) {
                // nothing was appended; the next call looks at the store again
                release_exclusive(&self.lock);
                self.held = false;
                self.blobs.end_batch();
            }
            if !ready? {
                self.reopen()?;
                again += 1;
                if again > 2 {
                    // the note of the lock file does not take what the
                    // marker says: what was read just now is gone by
                    self.generation = None;
                }
            }
        }
        Ok(())
    }

    /// With the write lock just taken: reads what other writers appended
    /// and bounds the store ([`Self::bound`]). `false`, with nothing read,
    /// where this handle has to read the store anew first: a removal of
    /// lost records was interrupted (`notes`) and is finished here, or
    /// the marker's generation is not the one this handle read the store
    /// at.
    fn caught_up(&mut self, note: Option<Left>, notes: repair::Notes) -> Result<bool, OdbError> {
        if notes.removal {
            repair::finish_held(&self.alt_dir, &self.lock)?;
            return Ok(false);
        }
        let marker = Marker::read(&self.durable_path);
        if self.generation.is_some_and(|at| at != marker.generation()) {
            // the handle that opens the store next reads the note
            repair::note_generation(&self.lock, marker.generation());
            return Ok(false);
        }
        self.catch_up()?;
        let note = self.as_written(&marker, note)?;
        self.settle_boot_runs(&marker, note, notes.boot_runs)?;
        self.bound(marker, note)?;
        Ok(true)
    }

    /// The `note` of appends left unsynced, for a writer that leaves its
    /// own so and has read the files to their ends: none where the note
    /// is of the running boot and a file ends in a record that does not
    /// read. What that boot left is then not as it was written: this
    /// writer reads the pack as one that syncs does, and from here on
    /// goes as after another boot, looking at every record past the
    /// boundaries and syncing what it keeps.
    fn as_written(
        &mut self,
        marker: &Marker,
        note: Option<Left>,
    ) -> Result<Option<Left>, OdbError> {
        let cached = alt_store::relaxed_durability() && unsynced::cached(marker, note);
        if !cached || !self.ends_unread()? {
            return Ok(note);
        }
        self.look()?;
        Ok(None)
    }

    /// Opens the store again in place of what this handle holds of it,
    /// for a handle that holds no lock; what belongs to the handle and
    /// not to the files stays.
    fn reopen(&mut self) -> Result<(), OdbError> {
        let (opts, late) = self.opened;
        let mode = Access::Write.mode(late, self.deferred.clone());
        let mut fresh = Odb::open_with(self.alt_dir.clone(), opts, mode)?;
        fresh.registry = std::mem::replace(&mut self.registry, default_registry());
        fresh.defer = self.defer;
        fresh.write_count = self.write_count;
        fresh.discarded = std::mem::take(&mut self.discarded);
        *self = fresh;
        Ok(())
    }

    /// Releases the write lock (best-effort; the fd closing also releases it).
    pub(super) fn release(&mut self) {
        if self.held {
            self.note_checked();
            if self.generation.is_some_and(|at| self.noted != Some(at)) {
                repair::note_generation(&self.lock, self.generation.unwrap_or_default());
                self.noted = self.generation;
            }
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
    /// holds. So are those of the running boot before a writer that
    /// syncs records them as synced: the cache holds them as they were
    /// written only while the file system under it is the one they were
    /// written to, and what a ref is to reach has to be there.
    ///
    /// `marker` and `note` are what the marker and the lock file said
    /// when the lock was taken.
    pub(super) fn bound(&mut self, marker: Marker, note: Option<Left>) -> Result<(), OdbError> {
        let relaxed = alt_store::relaxed_durability();
        *self.left.lock().unwrap() = note.filter(|_| relaxed);
        let cached = relaxed && unsynced::cached(&marker, note);
        // a pack noted as synced is not, once the marker stops saying so
        let unseen = unsynced::unseen(&marker, note);
        if !marker.trusted() || unseen.is_some() {
            if self.blobs.distrust_synced()? {
                head::forget_synced(&self.lock)?;
            } else if !relaxed && unsynced::cached(&marker, note) {
                // this handle read the pack as the running boot wrote it
                self.blobs.look_again()?;
            }
        }
        let looked_by = (!cached).then(|| unseen.as_ref().unwrap_or(&marker));
        self.settle(&marker, looked_by)?;
        let behind = !marker.trusted() && !(relaxed && cached);
        if behind {
            // what a cut made here took away the marker no longer counts
            let now = Marker::read(&self.durable_path);
            let synced = now.recorded().map(|state| state.eof);
            self.tier1_mut()?;
            let whole = self.appended_whole()?;
            self.sync_grown(synced, whole.seq)?;
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
        // the generation the store is at now: a cut moved it by one, and
        // a marker that recorded none was given one
        self.generation = Some(match (marker.recorded(), self.checked) {
            (Some(_), Some((generation, _))) => generation,
            _ => Marker::read(&self.durable_path).generation(),
        });
        Ok(())
    }
}
