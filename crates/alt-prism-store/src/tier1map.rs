//! Append-only map `blob id → recipe-record blob id` for Tier 1 blobs.
//! Same shape and crash story as the store's blobmap: a magic header, fixed
//! checksummed records, a torn final record truncated on open, a bad record
//! elsewhere reported as corruption. It is truth, not a cache.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

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
        } else {
            if valid_len < data.len() as u64 {
                file.set_len(valid_len)?;
                file.sync_all()?;
            }
            file.seek(SeekFrom::Start(valid_len))?;
        }
        Ok(Self { file, map })
    }

    pub fn append(&mut self, blob: BlobId, record: BlobId) -> Result<(), StoreError> {
        let mut rec = [0u8; REC_LEN];
        rec[..32].copy_from_slice(&blob.0);
        rec[32..64].copy_from_slice(&record.0);
        let check = checksum(&rec[..CHECKED_LEN]);
        rec[CHECKED_LEN..].copy_from_slice(&check);
        self.file.write_all(&rec)?;
        self.map.insert(blob, record);
        Ok(())
    }

    pub fn get(&self, blob: BlobId) -> Option<BlobId> {
        self.map.get(&blob).copied()
    }

    pub fn contains(&self, blob: BlobId) -> bool {
        self.map.contains_key(&blob)
    }

    pub fn sync(&mut self) -> Result<(), StoreError> {
        self.file.sync_all()?;
        Ok(())
    }
}

impl Drop for Tier1Map {
    fn drop(&mut self) {
        let _ = self.file.sync_all();
    }
}
