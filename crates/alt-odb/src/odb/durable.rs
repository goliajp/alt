//! Durability of the object database: the group-commit fsync and its
//! marker, write cursors, and rolling a batch back.

use std::fs::OpenOptions;

use super::Odb;
use crate::lock::{lock_exclusive, lock_shared, unlock};
use crate::synced::{self, Lens, Marker, State};
use crate::tier1::Tier1Map;
use crate::{OdbCheckpoint, OdbError};
use alt_store::StoreError;

impl Odb {
    /// Finish an unfinished object batch, including after a command error.
    /// Unlike `flush`, an idle call neither synchronizes nor counts a write.
    /// Daemons call this before handing a request to a durability barrier so
    /// a failed put cannot retain its exclusive lock across requests.
    pub fn flush_pending(&mut self) -> Result<(), OdbError> {
        if self.held {
            self.flush()?;
        }
        Ok(())
    }

    /// Durability point: blob store first (chunks before blobmap inside),
    /// then `map.alt`, so a crash never leaves a durable identity record
    /// pointing at lost content.
    pub fn flush(&mut self) -> Result<(), OdbError> {
        // my durability target = how far my appends reached in each file
        let target = self.appended()?;
        // read under the append lock: a rewind of this batch comes later
        // and moves it
        let generation = Marker::read(&self.durable_path).generation();
        // still under the append lock: fold what this batch (and any writer
        // before it) appended into the index runs, so the next open does not
        // have to scan it
        let indexed = self.index_tails();
        // release the exclusive append lock first, so other writers append (and
        // pile onto the same fsync) instead of waiting behind my durability
        self.release();
        indexed?;

        if alt_store::relaxed_durability() {
            return Ok(());
        }
        if self.defer {
            // the daemon's group-commit coordinator fsyncs off the write path
            // (via `sink`) and then records how far this batch reached
            self.write_count += 1;
            let whole = self.appended_whole()?;
            *self.deferred.lock().unwrap() = Some((whole, generation));
            return Ok(());
        }
        self.sync_to(target, generation)
    }

    /// With the write lock held and every file this handle has open read
    /// to its end: a store whose marker does not record boundaries that
    /// hold — an older alt wrote it, the marker was removed, or appends
    /// were left unsynced — gets them before anything is appended.
    pub(super) fn bound(&mut self) -> Result<(), OdbError> {
        let marker = Marker::read(&self.durable_path);
        if !alt_store::relaxed_durability() && !marker.trusted() {
            // the other two files are read to their ends as they open
            self.blobs.open_map()?;
            self.tier1()?;
            self.blobs.fsync()?;
            self.tier1_mut()?.sync()?;
            self.map.fsync()?;
            let whole = self.appended_whole()?;
            synced::establish(&self.durable_path, whole)?;
        }
        self.head.bounded = true;
        Ok(())
    }

    /// The record boundaries this handle's appends reached. A file it has
    /// not opened it has not appended to: zero, short of any length.
    fn appended(&self) -> Result<Lens, OdbError> {
        let (seq, pack, blobmap) = self.blobs.checkpoint()?.lens();
        Ok(Lens {
            seq,
            pack,
            blobmap,
            tier1: self.tier1.get().map_or(0, Tier1Map::appended_len),
            map: self.map.appended_len(),
        })
    }

    /// The same with every file opened: boundaries in all four.
    fn appended_whole(&self) -> Result<Lens, OdbError> {
        let tier1 = self.tier1()?.appended_len();
        Ok(Lens {
            tier1,
            ..self.appended()?
        })
    }

    /// Folds long enough log tails into the index runs. A run is fsynced
    /// as it is written, so the records it covers are fsynced first: no
    /// crash leaves a run over records that were lost. The runs are a
    /// cache: failing to write one is not an error, anything else is.
    pub(super) fn index_tails(&mut self) -> Result<(), OdbError> {
        let pending = self.blobs.index_pending()
            || self.tier1.get().is_some_and(|tier1| tier1.index_pending())
            || self.map.index_pending();
        if !self.held || !pending {
            return Ok(());
        }
        // indexing the blob store opens its map: opened here, it is
        // fsynced with the rest before a run is built over it
        self.blobs.open_map()?;
        if !alt_store::relaxed_durability() {
            self.blobs.fsync()?;
            if let Some(tier1) = self.tier1.get_mut() {
                tier1.sync()?;
            }
            self.map.fsync()?;
        }
        let indexed = (|| -> Result<(), StoreError> {
            self.blobs.index_tail()?;
            if let Some(tier1) = self.tier1.get_mut() {
                tier1.index_tail()?;
            }
            Ok(())
        })();
        match indexed
            .map_err(OdbError::from)
            .and_then(|()| self.map.index_tail())
        {
            Err(OdbError::Store(StoreError::IndexWrite(_))) => Ok(()),
            other => other,
        }
    }

    /// Snapshots the write cursor across all three append files (chunks pack,
    /// blobmap, map.alt) plus tier1, so a later [`Self::rewind`] can roll back
    /// every object written after it. Acquires the odb write lock + reconciles
    /// in-memory state with disk, captures the cursor, then releases the lock
    /// the same way [`Self::refresh`] does — keeping the file lock symmetric
    /// across request boundaries so a checkpoint at the start of a batch does
    /// not bleed into a long-held flock that blocks other readers/writers.
    pub fn checkpoint(&mut self) -> Result<OdbCheckpoint, OdbError> {
        self.acquire()?;
        let snapshot = self.cursor();
        self.release();
        snapshot
    }

    /// Cheap cursor snapshot of the in-memory write offsets, without touching
    /// the file lock or reconciling against the disk. Safe only when the
    /// caller already holds a write barrier (in-process: the store [`Mutex`];
    /// cross-process: the active flock from a prior [`Self::checkpoint`]) and
    /// just wants a post-batch snapshot to compare against the pre-batch one
    /// — i.e. "did the bytes I just wrote stay put, or did someone else's
    /// commit slip in?". Two extra syscalls saved per receive-pack request,
    /// which is what keeps the group-commit coalescing test still green
    /// with the rewind window in place.
    pub fn cursor(&self) -> Result<OdbCheckpoint, OdbError> {
        Ok(OdbCheckpoint {
            blobs: self.blobs.checkpoint()?,
            tier1_len: self.tier1()?.appended_len(),
            map_len: self.map.appended_len(),
        })
    }

    /// Rolls every odb write since `ckpt` back: truncates each append file to
    /// its captured length, drops the affected in-memory indices and fsyncs
    /// the new lengths. Used by the server's receive-pack path to undo a
    /// pack ingest when the signature / policy / ref-tx gate downstream
    /// rejects the push: no orphan objects are left in the store.
    ///
    /// Runs under the write lock, taken here unless the batch holds it.
    /// What stays of each file is read first: damage there fails the
    /// rewind with every file, index run and the marker as they were.
    pub fn rewind(&mut self, ckpt: OdbCheckpoint) -> Result<(), OdbError> {
        let held = self.held;
        self.acquire()?;
        let rewound = self.rewind_locked(ckpt);
        if !held {
            self.release();
        }
        rewound
    }

    fn rewind_locked(&mut self, ckpt: OdbCheckpoint) -> Result<(), OdbError> {
        self.blobs.check_rewind(ckpt.blobs)?;
        self.tier1()?.check_rewind(ckpt.tier1_len)?;
        self.map.check_rewind(ckpt.map_len)?;
        let (seq, pack, blobmap) = ckpt.blobs.lens();
        let after = Lens {
            seq,
            pack,
            blobmap,
            tier1: ckpt.tier1_len,
            map: ckpt.map_len,
        };
        // the marker comes down before the files do: it never claims more
        // than they hold, whichever step a crash interrupts. Its generation
        // moves, so a writer or a sink that wrote before the cut does not
        // record afterwards what the cut took
        synced::update(&self.durable_path, |marker| {
            let state = marker.recorded()?;
            Some(State {
                generation: state.generation + 1,
                eof: state.eof.min(after),
                whole: state.whole.min(after),
                ..*state
            })
        })?;
        self.blobs.rewind(ckpt.blobs)?;
        self.tier1_mut()?.rewind(ckpt.tier1_len)?;
        self.map.rewind(ckpt.map_len)?;
        // each cut was fsynced: the files are synced to exactly these lengths
        synced::update(&self.durable_path, |marker| {
            Some(match marker.recorded() {
                Some(state) => State {
                    eof: after,
                    whole: after,
                    ..*state
                },
                None => State {
                    generation: 1,
                    flags: 0,
                    eof: after,
                    whole: after,
                },
            })
        })?;
        Ok(())
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

    /// The fsync-coalescing core (blob store first — chunks before blobmap —
    /// then `tier1` (so the records are durable before any map.alt entry
    /// references their Tier 1 blob ids), then `map.alt`, so a crash never
    /// leaves a durable identity record pointing at lost content). One
    /// fsync covers my appends and any concurrent ones; the marker then
    /// lets the others skip theirs, and each of them still records how far
    /// its own records reach. `generation` is the marker's when `target`
    /// was read.
    fn sync_to(&mut self, target: Lens, generation: u64) -> Result<(), OdbError> {
        // fast path: another writer already fsynced past my appends
        let recorded = |marker: &Marker| {
            marker
                .recorded()
                .is_some_and(|state| state.generation == generation && state.whole.reaches(&target))
        };
        if recorded(&Marker::read(&self.durable_path)) {
            return Ok(());
        }
        let sync_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.sync_lock_path)?;
        lock_exclusive(&sync_lock)?;
        let result = (|| -> Result<(), OdbError> {
            let marker = Marker::read(&self.durable_path);
            if recorded(&marker) {
                return Ok(());
            }
            // a marker without record boundaries gets them for every file
            let target = match marker.recorded() {
                Some(_) => target,
                None => self.appended_whole()?,
            };
            // Otherwise a concurrent writer could append a map entry after
            // its content file was synchronized but before map.alt's fsync.
            // The batch released its exclusive lock before entering sync_to.
            lock_shared(&self.lock)?;
            let synced = self.sync_locked(target, generation);
            let _ = unlock(&self.lock);
            synced
        })();
        let _ = unlock(&sync_lock);
        result
    }

    /// Under the sync lock and the shared append lock (so no rewind and no
    /// writer's cut is under way): fsyncs unless one since the last rewind
    /// covered the files as far as `target`, and records it.
    fn sync_locked(&mut self, target: Lens, generation: u64) -> Result<(), OdbError> {
        let covered = Marker::read(&self.durable_path)
            .recorded()
            .is_some_and(|state| state.generation == generation && state.eof.reaches(&target));
        let eof = if covered {
            None
        } else {
            let (pack, blobmap) = self.blobs.file_lens()?;
            let eof = Lens {
                seq: target.seq,
                pack,
                blobmap,
                // not opened, not fsynced
                tier1: self.tier1.get().map_or(Ok(0), Tier1Map::file_len)?,
                map: self.map.file_len()?,
            };
            self.blobs.fsync()?; // chunks then blobmap, in order
            // then tier1 (references blob ids above)
            if let Some(tier1) = self.tier1.get_mut() {
                tier1.sync()?;
            }
            self.map.fsync()?; // then map.alt (may reference tier1 ids)
            Some(eof)
        };
        synced::record(&self.durable_path, generation, target, eof)?;
        Ok(())
    }
}
