//! Syncing a blob store's files: the chunk pack before the blob map.

use super::BlobStore;
use crate::StoreError;

impl BlobStore {
    /// Raw fsync of chunks then the blob map, in that order — so a crash never
    /// leaves a durable map record pointing at lost chunks. Group commit calls
    /// this once for a batch of coalesced commits.
    pub fn fsync(&mut self) -> Result<(), StoreError> {
        self.fsync_of(true, true)
    }

    /// [`Self::fsync`] of the files named: one that the caller knows to
    /// hold nothing past what an fsync already covered is left alone.
    pub fn fsync_of(&mut self, pack: bool, map: bool) -> Result<(), StoreError> {
        if pack {
            self.chunks.fsync()?;
        }
        match self.map.get_mut() {
            Some(held) if map => held.fsync(),
            _ => Ok(()),
        }
    }

    /// An independent fsync handle (chunks + blobmap) for the daemon's
    /// off-write-path group commit.
    pub fn sink(&self) -> Result<BlobSink, StoreError> {
        Ok(BlobSink {
            chunks: self.chunks.sink(),
            blobmap: self.map()?.sync_handle()?,
        })
    }
}

/// Off-write-path fsync handle for a [`BlobStore`]: the chunk pack then the blob
/// map, the same order [`BlobStore::fsync`] keeps so a crash never leaves a
/// durable map record pointing at lost chunks.
pub struct BlobSink {
    chunks: crate::ChunkSink,
    blobmap: std::fs::File,
}

impl BlobSink {
    pub fn fsync(&self) -> Result<(), StoreError> {
        self.fsync_of(true, true)
    }

    /// [`Self::fsync`] of the files named ([`BlobStore::fsync_of`]).
    pub fn fsync_of(&self, pack: bool, map: bool) -> Result<(), StoreError> {
        if pack {
            self.chunks.fsync()?;
        }
        if map {
            self.blobmap.sync_all()?;
        }
        Ok(())
    }
}
