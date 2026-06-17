//! Tier 1 bookkeeping for [`NativeOdb`](super::NativeOdb): an append-only
//! map `blob id → recipe-record blob id` plus the on-disk record layout that
//! ties a decomposed blob to its parts. Same shape and crash story as the
//! store's blobmap — a magic header, fixed checksummed records, a torn
//! final record truncated on open, any other corruption surfaced.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use alt_prism::PrismId;
use alt_store::{BlobId, StoreError};

const MAGIC: [u8; 4] = *b"ALT1";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 5;
/// Record: blob id 32 + record blob id 32 + checksum 8.
const REC_LEN: usize = 72;
const CHECKED_LEN: usize = REC_LEN - 8;

fn file_header() -> [u8; HEADER_LEN] {
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION]
}

fn checksum(checked: &[u8]) -> [u8; 8] {
    blake3::hash(checked).as_bytes()[..8].try_into().unwrap()
}

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
    file: File,
    map: HashMap<BlobId, BlobId>,
    /// Bytes flushed to disk (header + complete records); appended writes
    /// past this length stay buffered until [`sync`] drives the fsync.
    appended_len: u64,
}

impl Tier1Map {
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("tier1");

        let existing = match std::fs::read(&path) {
            Ok(data) => Some(data),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };

        let Some(data) = existing else {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)?;
            file.write_all(&file_header())?;
            file.sync_all()?;
            fsync_dir(dir)?;
            return Ok(Self {
                file,
                map: HashMap::new(),
                appended_len: HEADER_LEN as u64,
            });
        };

        if data.len() < HEADER_LEN {
            if !file_header().starts_with(&data) {
                return Err(StoreError::Format("bad tier1 header"));
            }
        } else if data[..4] != MAGIC {
            return Err(StoreError::Format("bad tier1 header"));
        } else if data[4] != VERSION {
            return Err(StoreError::Format("unsupported tier1 version"));
        }

        let mut map = HashMap::new();
        let mut at = HEADER_LEN.min(data.len());
        let mut valid_len = at as u64;
        while let Some(rec) = data.get(at..at + REC_LEN) {
            if checksum(&rec[..CHECKED_LEN]) != rec[CHECKED_LEN..] {
                if at + REC_LEN == data.len() {
                    break; // torn final record
                }
                return Err(StoreError::Format("tier1 record corrupt"));
            }
            let blob = BlobId(rec[..32].try_into().unwrap());
            let record = BlobId(rec[32..64].try_into().unwrap());
            map.insert(blob, record);
            at += REC_LEN;
            valid_len = at as u64;
        }

        let mut file = OpenOptions::new().write(true).open(&path)?;
        if valid_len < HEADER_LEN as u64 {
            file.set_len(0)?;
            file.write_all(&file_header())?;
            file.sync_all()?;
            valid_len = HEADER_LEN as u64;
        } else {
            if valid_len < data.len() as u64 {
                file.set_len(valid_len)?;
                file.sync_all()?;
            }
            file.seek(SeekFrom::Start(valid_len))?;
        }
        Ok(Self {
            file,
            map,
            appended_len: valid_len,
        })
    }

    pub fn append(&mut self, blob: BlobId, record: BlobId) -> Result<(), StoreError> {
        let mut rec = [0u8; REC_LEN];
        rec[..32].copy_from_slice(&blob.0);
        rec[32..64].copy_from_slice(&record.0);
        let check = checksum(&rec[..CHECKED_LEN]);
        rec[CHECKED_LEN..].copy_from_slice(&check);
        self.file.write_all(&rec)?;
        self.appended_len += REC_LEN as u64;
        self.map.insert(blob, record);
        Ok(())
    }

    pub fn get(&self, blob: BlobId) -> Option<BlobId> {
        self.map.get(&blob).copied()
    }

    pub fn contains(&self, blob: BlobId) -> bool {
        self.map.contains_key(&blob)
    }

    pub fn appended_len(&self) -> u64 {
        self.appended_len
    }

    pub fn sync(&mut self) -> Result<(), StoreError> {
        self.file.sync_all()?;
        Ok(())
    }

    /// Truncates the tier1 record back to `target_len`, removing every blob
    /// → record mapping appended past that point. Used by [`super::NativeOdb::rewind`]
    /// to roll back a failed write batch.
    pub fn rewind(&mut self, target_len: u64) -> Result<(), StoreError> {
        if target_len > self.appended_len {
            return Err(StoreError::Format("tier1 rewind target above cursor"));
        }
        if target_len < HEADER_LEN as u64 {
            return Err(StoreError::Format("tier1 rewind target below header"));
        }
        let drop_bytes = self.appended_len - target_len;
        if !drop_bytes.is_multiple_of(REC_LEN as u64) {
            return Err(StoreError::Format("tier1 rewind target mid-record"));
        }
        if drop_bytes > 0 {
            let mut tail = vec![0u8; drop_bytes as usize];
            self.file.seek(SeekFrom::Start(target_len))?;
            self.file.read_exact(&mut tail)?;
            for rec in tail.chunks_exact(REC_LEN) {
                let blob = BlobId(rec[..32].try_into().unwrap());
                self.map.remove(&blob);
            }
        }
        self.file.set_len(target_len)?;
        self.file.sync_all()?;
        self.file.seek(SeekFrom::Start(target_len))?;
        self.appended_len = target_len;
        Ok(())
    }

    /// An independent fd to the tier1 file for the daemon's off-write-path
    /// fsync — mirrors [`super::map::ObjectMap::sync_handle`].
    pub fn sync_handle(&self) -> Result<File, StoreError> {
        Ok(self.file.try_clone()?)
    }
}

impl Drop for Tier1Map {
    fn drop(&mut self) {
        let _ = self.file.sync_all();
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

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("tier1 record truncated")]
    Truncated,
    #[error("tier1 record has trailing bytes")]
    TrailingBytes,
}

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
