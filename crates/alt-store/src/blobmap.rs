//! Blob map: append-only record of blob id → manifest root. Unlike the
//! altidx this is truth, not cache — a blob's manifest root is not derivable
//! from the packs without materializing every manifest — so each record
//! carries its own checksum (see [`crate::reclog`] for how the log is opened
//! and indexed).

use std::fs::File;
use std::path::Path;

use crate::pack::fsync_dir;
use crate::reclog::{self, Layout, RecLog};
use crate::{BlobId, ChunkId, StoreError, Tail};

/// Record: blob id 32 + manifest root 32 + total_len u64 + checksum 8.
const REC_LEN: usize = 80;
const CHECKED_LEN: usize = REC_LEN - 8;

const LAYOUT: Layout = Layout {
    magic: *b"ALTB",
    version: 1,
    rec_len: REC_LEN,
    bad_header: "bad blobmap header",
    bad_version: "unsupported blobmap version",
    corrupt: "blobmap record corrupt",
    bad_rewind: "blobmap rewind target off a record boundary",
    keys: |rec| (rec[..32].try_into().unwrap(), None),
};

pub struct BlobMap {
    log: RecLog,
}

impl BlobMap {
    /// Opens `<dir>/blobmap`, creating it (and the directory) if missing;
    /// `named` are its index runs when the caller has them on record, and
    /// `tail` what a log that does not read to its end means.
    pub fn open_with_runs(
        dir: &Path,
        named: Option<&[(u64, u64)]>,
        tail: Tail,
    ) -> Result<Self, StoreError> {
        let (log, created) = RecLog::open_with_runs(&dir.join("blobmap"), LAYOUT, named, tail)?;
        if created {
            fsync_dir(dir)?;
        }
        Ok(Self { log })
    }

    pub fn append(
        &mut self,
        blob: BlobId,
        root: ChunkId,
        total_len: u64,
    ) -> Result<(), StoreError> {
        let mut rec = [0u8; REC_LEN];
        rec[..32].copy_from_slice(&blob.0);
        rec[32..64].copy_from_slice(&root.0);
        rec[64..72].copy_from_slice(&total_len.to_le_bytes());
        let check = reclog::checksum(&rec[..CHECKED_LEN]);
        rec[CHECKED_LEN..].copy_from_slice(&check);
        self.log.append(&rec)
    }

    /// Folds in records another writer appended since our last read. Run
    /// under the odb write lock. A torn tail left by a crashed writer is
    /// truncated (we hold the lock, so none is mid-append).
    pub fn sync_from_disk(&mut self) -> Result<(), StoreError> {
        self.log.sync_from_disk()
    }

    /// Indexes the records before byte `upto` that have no index run yet,
    /// once there are enough of them. Run under the odb write lock.
    pub fn index_tail(&mut self, upto: u64) -> Result<(), StoreError> {
        self.log.index_tail(upto, !crate::relaxed_durability())
    }

    pub fn index_pending(&self) -> bool {
        self.log.index_pending()
    }

    pub fn run_chain(&self) -> Vec<(u64, u64)> {
        self.log.run_chain()
    }

    pub fn get(&self, blob: BlobId) -> Result<Option<(ChunkId, u64)>, StoreError> {
        Ok(self.log.get(&blob.0)?.map(|rec| {
            let root = ChunkId(rec[32..64].try_into().unwrap());
            (root, u64::from_le_bytes(rec[64..72].try_into().unwrap()))
        }))
    }

    /// Full check of the log (see [`RecLog::check`]); `each` sees every
    /// blob recorded before byte `upto` and its total length.
    pub fn check(
        &self,
        upto: u64,
        mut each: impl FnMut(BlobId, u64) -> Result<(), StoreError>,
    ) -> Result<u64, StoreError> {
        self.log.check_upto(upto, |rec| {
            let blob = BlobId(rec[..32].try_into().unwrap());
            each(blob, u64::from_le_bytes(rec[64..72].try_into().unwrap()))
        })
    }

    /// Visits the records from byte `from` of the log on: where each
    /// begins, the blob and its manifest root.
    pub fn each_from(
        &self,
        from: u64,
        mut each: impl FnMut(u64, BlobId, ChunkId) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.log.each_from(from, |at, rec| {
            let blob = BlobId(rec[..32].try_into().unwrap());
            each(at, blob, ChunkId(rec[32..64].try_into().unwrap()))
        })
    }

    /// See [`RecLog::unfinished`].
    pub fn unfinished(&self) -> bool {
        self.log.unfinished()
    }

    /// See [`RecLog::passed`].
    pub fn passed(&self) -> Result<crate::Passed, StoreError> {
        self.log.passed()
    }

    pub fn contains(&self, blob: BlobId) -> Result<bool, StoreError> {
        self.log.contains(&blob.0)
    }

    pub fn sync(&mut self) -> Result<(), StoreError> {
        self.log.sync()
    }

    /// Raw fsync (no relaxed gate) — called by the group commit layer.
    pub fn fsync(&self) -> Result<(), StoreError> {
        self.log.fsync()
    }

    /// Bytes we have appended (our write cursor).
    pub fn appended_len(&self) -> u64 {
        self.log.appended_len()
    }

    /// The blobmap's true on-disk size.
    pub fn file_len(&self) -> Result<u64, StoreError> {
        self.log.file_len()
    }

    /// Truncates the blobmap back to `target_len` (a previous `appended_len`),
    /// dropping every record appended past that point. Followed by an fsync
    /// so the truncation is durable: callers (the odb rewind path) rely on
    /// this to be torn-tail-free on recovery.
    /// Whether [`Self::rewind`] to `target_len` can go ahead (see
    /// [`RecLog::check_rewind`]).
    pub fn check_rewind(&self, target_len: u64) -> Result<(), StoreError> {
        if target_len > self.log.appended_len() {
            return Err(StoreError::Format("blobmap rewind target above cursor"));
        }
        self.log.check_rewind(target_len).map(drop)
    }

    pub fn rewind(&mut self, target_len: u64) -> Result<(), StoreError> {
        if target_len > self.log.appended_len() {
            return Err(StoreError::Format("blobmap rewind target above cursor"));
        }
        self.log.rewind(target_len)
    }

    /// An independent fd to the (stable, never-rolled) blobmap file, for the
    /// daemon's off-write-path fsync. `fsync` flushes the inode regardless of
    /// which fd it is called on, so this dup is enough.
    pub fn sync_handle(&self) -> Result<File, StoreError> {
        self.log.sync_handle()
    }
}
