//! Reads that outlive a compaction.
//!
//! A reader holds no lock while it reads, and maps a sealed pack only when
//! a read first lands in it. A compaction by another process meanwhile
//! replaces every pack: the ones this handle has not mapped yet are then
//! gone from the directory. The chunks are all still there, in the
//! compacted pack, so a read that finds a pack missing loads the store
//! again and is answered from that.

use std::path::PathBuf;

use crate::pack::with_shared_lock;
use crate::{ChunkId, ChunkStat, ChunkStore, StoreError, Verify};

fn pack_gone(error: &StoreError) -> bool {
    matches!(error, StoreError::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
}

impl ChunkStore {
    /// Names the lock file writers of this store append under, so a reload
    /// waits for a writer (a compaction above all) to finish.
    pub(crate) fn guard_opens(&mut self, lock: PathBuf) -> PathBuf {
        self.open_lock = Some(lock.clone());
        lock
    }

    /// The most recently loaded state of this store.
    fn newest(&self) -> &ChunkStore {
        let mut store = self;
        while let Some(newer) = store.newer.get() {
            store = newer;
        }
        store
    }

    /// Loads the store again after `self` (the newest state so far) found
    /// one of its packs gone.
    fn reloaded(&self) -> Result<&ChunkStore, StoreError> {
        let open = || ChunkStore::open_with(&self.dir, self.opts);
        let mut fresh = match &self.open_lock {
            Some(lock) => with_shared_lock(lock, open)?,
            None => open()?,
        };
        fresh.open_lock = self.open_lock.clone();
        if self.keep {
            fresh.keep_reads();
        }
        Ok(self.newer.get_or_init(|| Box::new(fresh)))
    }

    /// Runs `read` against the newest state, loading a newer one for as
    /// long as it fails on a pack that is gone.
    pub(crate) fn reading<T>(
        &self,
        mut read: impl FnMut(&ChunkStore) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut store = self.newest();
        loop {
            match read(store) {
                Err(e) if pack_gone(&e) => store = store.reloaded()?,
                done => return done,
            }
        }
    }

    /// The newest state with every sealed pack mapped: nothing a compaction
    /// does later can take a pack away from a read that started on it.
    pub(crate) fn pinned(&self) -> Result<&ChunkStore, StoreError> {
        let mut store = self.newest();
        loop {
            // what follows reads the packs from end to end
            let mapped = store
                .sealed
                .keys()
                .try_for_each(|seq| store.sealed_pack(*seq).map(|_| store.read_ahead(*seq)));
            match mapped {
                Err(e) if pack_gone(&e) => store = store.reloaded()?,
                Err(e) => return Err(e),
                Ok(()) => return Ok(store),
            }
        }
    }

    pub(crate) fn resolve(&self, id: ChunkId, verify: Verify) -> Result<Vec<u8>, StoreError> {
        self.reading(|store| store.resolve_here(id, verify))
    }

    /// Decodes a chunk with no hashing at all (see the blob store's bulk
    /// read).
    pub(crate) fn decode_chunk_unverified(&self, id: ChunkId) -> Result<Vec<u8>, StoreError> {
        self.reading(|store| store.decode_unverified_here(id))
    }

    /// On-disk accounting for one chunk (encoding and stored size).
    pub fn stat(&self, id: ChunkId) -> Result<ChunkStat, StoreError> {
        self.reading(|store| store.stat_here(id))
    }

    /// Decodes every stored chunk once and hands it to `f`, bases before
    /// the deltas made against them.
    pub fn for_each_decoded(&self, f: impl FnMut(ChunkId, &[u8])) -> Result<(), StoreError> {
        self.pinned()?.each_decoded_here(f)
    }
}
