//! Folding a chunk store's newly appended records into its index runs.

use crate::packs::{self, load};
use crate::{ChunkStore, StoreError, relaxed_durability};

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

    /// Whether enough records sit past the index runs for
    /// [`Self::index_tail`] to act.
    pub fn index_pending(&self) -> bool {
        self.index.pending().is_some()
    }
}
