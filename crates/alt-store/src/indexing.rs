//! Folding a chunk store's newly appended records into its index runs.

use crate::index::Location;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use crate::packs::{self, Known, load};
use crate::{ChunkId, ChunkStore, Counters, DeltaCache, Options, StoreError, Tail};

impl ChunkStore {
    /// Opens the store in `dir`; `known` names its packs and index runs
    /// when the caller has them on record (see [`Known`]); `tail` says what
    /// to do about a pack that does not read to its end.
    pub fn open_known(
        dir: impl Into<PathBuf>,
        opts: Options,
        known: Option<&Known>,
        tail: Tail,
    ) -> Result<Self, StoreError> {
        let dir = dir.into();
        let ((sealed, active, index), fresh) = load(&dir, known, &tail)?;
        let mut store = Self {
            dir,
            opts,
            sealed,
            active,
            index,
            unsynced: false,
            counters: Counters::default(),
            cache: Mutex::new(DeltaCache::new()),
            open_lock: None,
            newer: OnceLock::new(),
            keep: false,
            tail,
        };
        store.settle(fresh, &store.tail.clone())?;
        Ok(store)
    }

    /// Reads the packs and the index again, for the holder of the write
    /// lock: an unfinished record at the end of the active pack is cut off.
    pub(crate) fn reload(&mut self) -> Result<(), StoreError> {
        let tail = self.tail.clone().locked();
        let (state, fresh) = load(&self.dir, None, &tail)?;
        self.reload_from(state)?;
        self.settle(fresh, &tail)
    }

    /// The same, for the holder of the write lock that has already folded
    /// in what other writers appended: no record at the end of the active
    /// pack can be a torn one, so a pack that does not read to its end is
    /// reported as damaged and nothing is cut off it.
    pub(crate) fn reload_whole(&mut self) -> Result<(), StoreError> {
        let tail = self.tail.clone().locked();
        self.reload_from(packs::load_from(&self.dir, None, true, None, &tail)?.0)
    }

    pub(crate) fn reload_from(&mut self, state: packs::PackState) -> Result<(), StoreError> {
        let (sealed, active, index) = state;
        self.sealed = sealed;
        self.active = active;
        self.index = index;
        if self.keep {
            self.index.keep_reads();
        }
        Ok(())
    }

    /// For a handle that lives long and reads the same chunks again and
    /// again: where the index found each one is kept in memory from now on.
    pub fn keep_reads(&mut self) {
        self.keep = true;
        self.index.keep_reads();
    }

    /// Pack sequence numbers, ascending; the last is the active pack.
    pub fn pack_seqs(&self) -> Vec<u32> {
        self.sealed
            .keys()
            .copied()
            .chain([self.active.seq])
            .collect()
    }

    /// The chunk index's runs (see [`crate::runs::Runs::open_named`]).
    pub fn run_chain(&self) -> Vec<(u64, u64)> {
        self.index.chain()
    }

    /// Moves the records appended since the last index run into a new run,
    /// once there are enough of them. Run under the odb write lock, after
    /// `sync_from_disk`, so every record on disk is known here.
    pub fn index_tail(&mut self) -> Result<(), StoreError> {
        if self.index_from_tail(u64::MAX)? {
            return Ok(());
        }
        // the runs on disk stop short of where this handle's tail begins
        // (their directory was removed while it was open): read the packs
        // again from where they stop now, then index
        self.reload_whole()?;
        self.index_from_tail(u64::MAX).map(drop)
    }

    /// The same for the records before byte `upto` of the active pack and
    /// no others, for a store read under the write lock by an opener that
    /// is not to sync it: the records a run names were synced.
    pub fn index_synced(&mut self, upto: u64) -> Result<(), StoreError> {
        let (seq, _) = self.checkpoint().pack();
        let upto = Location { seq, offset: upto };
        self.index_from_tail(upto.position()).map(drop)
    }

    /// Whether [`Self::index_synced`] would build a run, told the same.
    pub fn synced_pending(&self, upto: u64) -> bool {
        let (seq, _) = self.checkpoint().pack();
        let upto = Location { seq, offset: upto };
        self.index.pending(upto.position()).is_some()
    }

    fn index_from_tail(&mut self, upto: u64) -> Result<bool, StoreError> {
        let Some(end) = self.index.pending(upto) else {
            return Ok(true);
        };
        let dir = &self.dir;
        let guard = match packs::guard_at(dir, end) {
            Some(guard) => guard,
            // no run ends where a pack begins: the synced records stop there
            None if upto != u64::MAX => return Ok(true),
            None => return Err(StoreError::Format("pack shorter than its index")),
        };
        self.index.index_tail(
            end,
            guard,
            |loc| packs::guard_at(dir, loc),
            crate::runs::written_synced(),
        )
    }

    /// Where `id` is stored. An answer that comes from the index runs is
    /// confirmed against the pack: the runs are a cache whose guard covers
    /// only the end of what they index.
    pub(crate) fn located(&self, id: ChunkId) -> Result<Option<Location>, StoreError> {
        let Some((loc, from_runs)) = self.index.find(id)? else {
            return Ok(None);
        };
        if from_runs {
            self.read_header(id, loc)?;
        }
        Ok(Some(loc))
    }

    /// Whether `id` is stored, for a writer about to skip writing it. Runs
    /// that name a record which is not there are dropped and the index
    /// built again from the packs: a write is never skipped on their word.
    pub(crate) fn stored(&mut self, id: ChunkId) -> Result<bool, StoreError> {
        match self.located(id) {
            Ok(loc) => Ok(loc.is_some()),
            Err(StoreError::Corrupt { .. } | StoreError::Io(_)) => {
                self.index.clear()?;
                self.reload_whole()?;
                Ok(self.index.get(id)?.is_some())
            }
            Err(e) => Err(e),
        }
    }

    /// Whether enough records sit past the index runs for
    /// [`Self::index_tail`] to act.
    pub fn index_pending(&self) -> bool {
        self.index.pending(u64::MAX).is_some()
    }
}
