//! What the holder of the writers' lock needs to deal with records that
//! name content the store does not hold: after a power cut a blob map
//! record, or a record of a log above this store, can be on disk while the
//! chunks it names are not. Nothing here hashes anything: a chunk is
//! stored when the index finds it.

use super::BlobStore;
use crate::node::parse_node;
use crate::{BlobId, ChunkId, StoreError};

impl BlobStore {
    /// Whether an append that never finished follows the records held
    /// here: in the active pack, and in the blob map.
    pub fn unfinished(&self) -> Result<(bool, bool), StoreError> {
        Ok((self.chunks.unfinished(), self.map()?.unfinished()))
    }

    /// The file of the blob map.
    pub fn map_path(&self) -> std::path::PathBuf {
        self.dir.join("manifests").join("blobmap")
    }

    /// The file of the active pack.
    pub fn active_pack_path(&self) -> std::path::PathBuf {
        self.chunks.active_path()
    }

    /// See [`crate::ChunkStore::distrust_synced`].
    pub fn distrust_synced(&mut self) -> Result<bool, StoreError> {
        self.chunks.distrust_synced()
    }

    /// Visits the blob map's records from byte `from` on: where each
    /// begins, the blob and its manifest root.
    pub fn map_records_from(
        &self,
        from: u64,
        each: impl FnMut(u64, BlobId, ChunkId) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.map()?.each_from(from, each)
    }

    /// Whether the chunk a blob of one chunk is stored as is there.
    pub fn chunk_stored(&self, id: BlobId) -> Result<bool, StoreError> {
        self.chunks.contains(ChunkId(id.0))
    }

    /// Whether the blob map names `id`.
    pub fn mapped(&self, id: BlobId) -> Result<bool, StoreError> {
        self.map()?.contains(id)
    }

    /// Whether every chunk of the manifest tree under `root` is stored.
    pub fn tree_stored(&self, root: ChunkId) -> Result<bool, StoreError> {
        self.subtree_stored(root, None)
    }

    fn subtree_stored(&self, node: ChunkId, expect: Option<u8>) -> Result<bool, StoreError> {
        let bytes = match self.chunks.read_unverified(node) {
            Err(StoreError::NotFound(_)) => return Ok(false),
            read => read?,
        };
        let (level, entries) = parse_node(&bytes)?;
        if expect.is_some_and(|expect| level != expect) {
            return Err(StoreError::Format("manifest level mismatch"));
        }
        for entry in entries {
            let stored = match level {
                0 => self.chunks.contains(entry.id)?,
                _ => self.subtree_stored(entry.id, Some(level - 1))?,
            };
            if !stored {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Cuts the blob map back to `map_len` and an append that never
    /// finished off the active pack, in that order, each cut fsynced.
    /// The caller has brought down what is recorded of how far the two
    /// were synced.
    pub fn cut_back(&mut self, map_len: u64) -> Result<(), StoreError> {
        let map = self.map_mut()?;
        if map.unfinished() || map_len < map.appended_len() {
            map.rewind(map_len)?;
        }
        if self.chunks.unfinished() {
            self.chunks.cut_unfinished()?;
        }
        Ok(())
    }
}
