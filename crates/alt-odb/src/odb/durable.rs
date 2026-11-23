//! Durability of the object database: the group-commit fsync and its
//! marker, write cursors, and rolling a batch back.

use std::fs::OpenOptions;

use super::Odb;
use crate::head;
use crate::lock::{lock_exclusive, lock_shared, unlock};
use crate::synced::{self, Lens, Marker, State};
use crate::tier1::Tier1Map;
use crate::{OdbCheckpoint, OdbError};

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
        let target = self.appended();
        // read under the append lock: a rewind of this batch comes later
        // and moves it
        let generation = Marker::read(&self.durable_path).generation();
        // still under the append lock: fold what this batch (and any writer
        // before it) appended into the index runs, so the next open does not
        // have to scan it
        let indexed = self.index_tails();
        let relaxed = alt_store::relaxed_durability();
        // the daemon's group-commit coordinator fsyncs off the write path
        // (via `sink`) and then records how far this batch reached. It is
        // told before the lock is given up: a sink syncs under the shared
        // lock and so never records less than the batches that ended
        let deferred = match self.defer && !relaxed {
            true => Some(self.appended_whole()),
            false => None,
        };
        if let Some(whole) = &deferred {
            let mut left = self.deferred.lock().unwrap();
            match (whole, &target, &indexed) {
                (Ok(whole), Ok(_), Ok(())) => left.whole = Some((*whole, generation)),
                _ => left.failed = true,
            }
        }
        // release the exclusive append lock first, so other writers append (and
        // pile onto the same fsync) instead of waiting behind my durability
        self.release();
        let target = target?;
        indexed?;

        if relaxed {
            return Ok(());
        }
        if let Some(whole) = deferred {
            whole?;
            self.write_count += 1;
            return Ok(());
        }
        self.sync_to(target, generation)
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
    pub(super) fn appended_whole(&self) -> Result<Lens, OdbError> {
        let tier1 = self.tier1()?.appended_len();
        Ok(Lens {
            tier1,
            ..self.appended()?
        })
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
        self.end_boot_runs()?;
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
        head::forget_synced(&self.lock)?;
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
        // what names content is cut before the content it names: whichever
        // step a crash interrupts, no record is left naming what was cut
        self.map.rewind(ckpt.map_len)?;
        self.tier1_mut()?.rewind(ckpt.tier1_len)?;
        self.blobs.rewind(ckpt.blobs)?;
        // each cut was fsynced: the files are synced to exactly these lengths
        synced::update(&self.durable_path, |marker| {
            Some(match marker.recorded() {
                Some(state) => State {
                    eof: after,
                    whole: after,
                    ..*state
                },
                None => State {
                    generation: super::generation_kept(marker),
                    flags: 0,
                    eof: after,
                    whole: after,
                },
            })
        })?;
        head::note_synced(&self.lock, after);
        self.generation = Some(Marker::read(&self.durable_path).generation());
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
}
