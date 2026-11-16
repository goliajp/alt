//! Opening a blob store, and its blob map on first use.

use std::path::PathBuf;
use std::sync::OnceLock;

use super::{BlobOptions, BlobStore};
use crate::blobmap::BlobMap;
use crate::packs::Known;
use crate::{ChunkStore, StoreError, Tail};

impl BlobStore {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        Self::open_with(dir, BlobOptions::default())
    }

    pub fn open_with(dir: impl Into<PathBuf>, opts: BlobOptions) -> Result<Self, StoreError> {
        let store = Self::open_deferred(dir, opts, None, [Tail::default(), Tail::default()])?;
        store.map()?;
        Ok(store)
    }

    /// Opens the chunk store and leaves the blob map for the first call
    /// that needs it — which is then also where a damaged map is reported.
    /// `known` names the store's packs and index runs when the caller has
    /// them on record, which saves reading the directories. `tails` say
    /// what a chunk pack and the blob map that do not read to their ends
    /// mean, in that order.
    pub fn open_deferred(
        dir: impl Into<PathBuf>,
        opts: BlobOptions,
        known: Option<&Known>,
        tails: [Tail; 2],
    ) -> Result<Self, StoreError> {
        let [chunk_tail, map_tail] = tails;
        assert!(opts.fanout >= 2, "manifest fanout must be at least 2");
        let dir = dir.into();
        let chunks = ChunkStore::open_known(&dir, opts.chunks, known, chunk_tail)?;
        Ok(Self {
            chunks,
            map: OnceLock::new(),
            map_runs: known.and_then(|known| known.map_runs.clone()),
            map_tail,
            dir,
            opts,
            open_lock: None,
            exclusive: false,
        })
    }

    /// Names the lock file writers of this store append under; the store
    /// must have been opened under that lock, shared.
    pub fn guard_opens(&mut self, lock: PathBuf) {
        self.open_lock = Some(self.chunks.guard_opens(lock));
    }

    /// The write batch that began with `sync_from_disk` is over and its lock
    /// released.
    pub fn end_batch(&mut self) {
        self.exclusive = false;
    }

    pub(crate) fn map(&self) -> Result<&BlobMap, StoreError> {
        if let Some(map) = self.map.get() {
            return Ok(map);
        }
        let open = || {
            let runs = self.map_runs.as_deref();
            BlobMap::open_with_runs(&self.dir.join("manifests"), runs, self.map_tail.clone())
        };
        let map = match &self.open_lock {
            Some(lock) if !self.exclusive => crate::pack::with_shared_lock(lock, open)?,
            _ => open()?,
        };
        Ok(self.map.get_or_init(|| map))
    }

    /// For a handle that lives long: chunk locations found through the
    /// index are kept in memory from now on (see
    /// [`ChunkStore::keep_reads`]).
    pub fn keep_reads(&mut self) {
        self.chunks.keep_reads();
    }

    /// Opens the blob map now rather than on first use.
    pub fn open_map(&self) -> Result<(), StoreError> {
        self.map().map(drop)
    }

    pub(super) fn map_mut(&mut self) -> Result<&mut BlobMap, StoreError> {
        self.map()?;
        Ok(self
            .map
            .get_mut()
            .expect("the blob map was opened just above"))
    }

    /// What the next opener can be told about this store's files (see
    /// [`Known`]). The blob map's runs are reported only once the map has
    /// been opened: until then they are whatever they were.
    pub fn known(&self) -> Known {
        Known {
            packs: Some(self.chunks.pack_seqs()),
            chunk_runs: Some(self.chunks.run_chain()),
            map_runs: self.map.get().map(BlobMap::run_chain),
        }
    }
}
