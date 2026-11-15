//! Folding a chunk store's newly appended records into its index runs.

use crate::index::Location;
use crate::packs::{self, load};
use crate::{ChunkId, ChunkStore, StoreError, relaxed_durability};

impl ChunkStore {
    /// Moves the records appended since the last index run into a new run,
    /// once there are enough of them. Run under the odb write lock, after
    /// `sync_from_disk`, so every record on disk is known here.
    pub fn index_tail(&mut self) -> Result<(), StoreError> {
        if self.index_from_tail()? {
            return Ok(());
        }
        // the runs on disk stop short of where this handle's tail begins
        // (their directory was removed while it was open): read the packs
        // again from where they stop now, then index
        let (sealed, active, index) = load(&self.dir)?;
        self.sealed = sealed;
        self.active = active;
        self.index = index;
        self.index_from_tail().map(drop)
    }

    fn index_from_tail(&mut self) -> Result<bool, StoreError> {
        let Some(end) = self.index.pending() else {
            return Ok(true);
        };
        let dir = &self.dir;
        let guard =
            packs::guard_at(dir, end).ok_or(StoreError::Format("pack shorter than its index"))?;
        self.index.index_tail(
            guard,
            |loc| packs::guard_at(dir, loc),
            !relaxed_durability(),
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
                let (sealed, active, index) = load(&self.dir)?;
                self.sealed = sealed;
                self.active = active;
                self.index = index;
                Ok(self.index.get(id)?.is_some())
            }
            Err(e) => Err(e),
        }
    }

    /// Whether enough records sit past the index runs for
    /// [`Self::index_tail`] to act.
    pub fn index_pending(&self) -> bool {
        self.index.pending().is_some()
    }
}
