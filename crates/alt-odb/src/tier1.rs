//! Tier 1 bookkeeping for [`NativeOdb`](super::NativeOdb): an append-only
//! map `blob id → recipe-record blob id` plus the on-disk record layout that
//! ties a decomposed blob to its parts. Same shape and crash story as the
//! store's blobmap — a magic header, fixed checksummed records, a torn
//! final record truncated on open, any other corruption surfaced.

use std::fs::File;
use std::path::Path;

use alt_prism::PrismId;
use alt_store::reclog::{self, Layout, RecLog};
use alt_store::{BlobId, StoreError};

/// Record: blob id 32 + record blob id 32 + checksum 8.
const REC_LEN: usize = 72;
const CHECKED_LEN: usize = REC_LEN - 8;

const LAYOUT: Layout = Layout {
    magic: *b"ALT1",
    version: 1,
    rec_len: REC_LEN,
    bad_header: "bad tier1 header",
    bad_version: "unsupported tier1 version",
    corrupt: "tier1 record corrupt",
    bad_rewind: "tier1 rewind target off a record boundary",
    keys: |rec| (rec[..32].try_into().unwrap(), None),
};

/// Durability of a file create/rename needs the directory entry on disk too.
fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

pub(crate) struct Tier1Map {
    log: RecLog,
}

impl Tier1Map {
    /// Opens `<dir>/tier1`, creating it if missing; `named` are its index
    /// runs when the caller has them on record.
    pub fn open(
        dir: &Path,
        named: Option<&[(u64, u64)]>,
        tail: alt_store::Tail,
    ) -> Result<Self, StoreError> {
        let (log, created) = RecLog::open_with_runs(&dir.join("tier1"), LAYOUT, named, tail)?;
        if created {
            fsync_dir(dir)?;
        }
        Ok(Self { log })
    }

    pub fn append(&mut self, blob: BlobId, record: BlobId) -> Result<(), StoreError> {
        let mut rec = [0u8; REC_LEN];
        rec[..32].copy_from_slice(&blob.0);
        rec[32..64].copy_from_slice(&record.0);
        let check = reclog::checksum(&rec[..CHECKED_LEN]);
        rec[CHECKED_LEN..].copy_from_slice(&check);
        self.log.append(&rec)
    }

    /// Folds in records another writer appended since our last read. Run
    /// under the odb write lock.
    pub fn sync_from_disk(&mut self) -> Result<(), StoreError> {
        self.log.sync_from_disk()
    }

    /// Indexes the records before byte `upto` that have no index run yet,
    /// once there are enough of them. Run under the odb write lock.
    pub fn index_tail(&mut self, upto: u64) -> Result<(), StoreError> {
        self.log.index_tail(upto, !alt_store::relaxed_durability())
    }

    pub fn index_pending(&self) -> bool {
        self.log.index_pending()
    }

    pub fn get(&self, blob: BlobId) -> Result<Option<BlobId>, StoreError> {
        Ok(self
            .log
            .get(&blob.0)?
            .map(|rec| BlobId(rec[32..64].try_into().unwrap())))
    }

    /// Full check of the log (see [`RecLog::check`]); `each` sees every
    /// decomposed blob recorded before byte `upto`.
    pub fn check<E: From<StoreError>>(
        &self,
        upto: u64,
        mut each: impl FnMut(BlobId) -> Result<(), E>,
    ) -> Result<u64, E> {
        self.log
            .check_upto(upto, |rec| each(BlobId(rec[..32].try_into().unwrap())))
    }

    /// Visits the records from byte `from` of the log on: where each
    /// begins, the decomposed blob and the blob its recipe record is.
    pub fn each_from<E: From<StoreError>>(
        &self,
        from: u64,
        mut each: impl FnMut(u64, BlobId, BlobId) -> Result<(), E>,
    ) -> Result<(), E> {
        self.log.each_from(from, |at, rec| {
            let blob = BlobId(rec[..32].try_into().unwrap());
            each(at, blob, BlobId(rec[32..64].try_into().unwrap()))
        })
    }

    /// See [`RecLog::pass_over_from`].
    pub fn pass_over_from(&mut self, at: u64) {
        self.log.pass_over_from(at)
    }

    /// See [`RecLog::unfinished`].
    pub fn unfinished(&self) -> bool {
        self.log.unfinished()
    }

    /// See [`RecLog::passed`].
    pub fn passed(&self) -> Result<alt_store::Passed, StoreError> {
        self.log.passed()
    }

    pub fn run_chain(&self) -> Vec<(u64, u64)> {
        self.log.run_chain()
    }

    pub fn contains(&self, blob: BlobId) -> Result<bool, StoreError> {
        self.log.contains(&blob.0)
    }

    pub fn appended_len(&self) -> u64 {
        self.log.appended_len()
    }

    pub fn sync(&mut self) -> Result<(), StoreError> {
        self.log.fsync()
    }

    /// The file's true on-disk size.
    pub fn file_len(&self) -> Result<u64, StoreError> {
        self.log.file_len()
    }

    /// Truncates the tier1 record back to `target_len`, removing every blob
    /// → record mapping appended past that point. Used by [`super::NativeOdb::rewind`]
    /// to roll back a failed write batch.
    pub fn rewind(&mut self, target_len: u64) -> Result<(), StoreError> {
        self.check_rewind(target_len)?;
        self.log.rewind(target_len)
    }

    /// Whether [`Self::rewind`] to `target_len` can go ahead (see
    /// [`RecLog::check_rewind`]).
    pub fn check_rewind(&self, target_len: u64) -> Result<(), StoreError> {
        if target_len > self.log.appended_len() {
            return Err(StoreError::Format("tier1 rewind target above cursor"));
        }
        self.log.check_rewind(target_len).map(drop)
    }

    /// An independent fd to the tier1 file for the daemon's off-write-path
    /// fsync — mirrors [`super::map::ObjectMap::sync_handle`].
    pub fn sync_handle(&self) -> Result<File, StoreError> {
        self.log.sync_handle()
    }
}

/// Record layout shared with [`alt_prism_store`](alt_prism_store): `[prism
/// u16][recipe_len u32][recipe][part_count u32][part ids 32 each]`. Encoded
/// once per Tier 1 acceptance and stored as a normal blob in the underlying
/// chunk store — so it deduplicates against identical recipes and rides the
/// same crash story as every other blob.
pub fn encode_record(prism: PrismId, recipe: &[u8], parts: &[BlobId]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + 4 + recipe.len() + 4 + parts.len() * 32);
    out.extend_from_slice(&prism.0.to_le_bytes());
    out.extend_from_slice(&(recipe.len() as u32).to_le_bytes());
    out.extend_from_slice(recipe);
    out.extend_from_slice(&(parts.len() as u32).to_le_bytes());
    for p in parts {
        out.extend_from_slice(&p.0);
    }
    out
}

#[derive(Debug)]
pub enum RecordError {
    Truncated,
    TrailingBytes,
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => f.write_str("tier1 record truncated"),
            Self::TrailingBytes => f.write_str("tier1 record has trailing bytes"),
        }
    }
}

impl std::error::Error for RecordError {}

pub fn decode_record(rec: &[u8]) -> Result<(PrismId, Vec<u8>, Vec<BlobId>), RecordError> {
    let err = || RecordError::Truncated;
    let prism = PrismId(u16::from_le_bytes(
        rec.get(..2).ok_or_else(err)?.try_into().unwrap(),
    ));
    let recipe_len =
        u32::from_le_bytes(rec.get(2..6).ok_or_else(err)?.try_into().unwrap()) as usize;
    let recipe = rec.get(6..6 + recipe_len).ok_or_else(err)?.to_vec();
    let mut at = 6 + recipe_len;
    let count =
        u32::from_le_bytes(rec.get(at..at + 4).ok_or_else(err)?.try_into().unwrap()) as usize;
    at += 4;
    let mut parts = Vec::with_capacity(count);
    for _ in 0..count {
        let bytes = rec.get(at..at + 32).ok_or_else(err)?;
        parts.push(BlobId(bytes.try_into().unwrap()));
        at += 32;
    }
    if at != rec.len() {
        return Err(RecordError::TrailingBytes);
    }
    Ok((prism, recipe, parts))
}
