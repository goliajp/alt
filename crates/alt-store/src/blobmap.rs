//! Blob map: append-only record of blob id → manifest root. Unlike the
//! altidx this is truth, not cache — a blob's manifest root is not derivable
//! from the packs without materializing every manifest — so each record
//! carries its own checksum. A torn tail (crash mid-append) is truncated on
//! open; a bad record anywhere else is corruption and reported as such.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::pack::fsync_dir;
use crate::{BlobId, ChunkId, StoreError};

const MAGIC: [u8; 4] = *b"ALTB";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 5;
/// Record: blob id 32 + manifest root 32 + total_len u64 + checksum 8.
const REC_LEN: usize = 80;
const CHECKED_LEN: usize = REC_LEN - 8;

fn file_header() -> [u8; HEADER_LEN] {
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION]
}

fn checksum(checked: &[u8]) -> [u8; 8] {
    blake3::hash(checked).as_bytes()[..8].try_into().unwrap()
}

pub struct BlobMap {
    file: File,
    map: HashMap<BlobId, (ChunkId, u64)>,
    /// Byte offset past the last record we have read — lets `sync_from_disk`
    /// fold in only the records another writer appended since.
    len: u64,
}

impl BlobMap {
    /// Opens `<dir>/blobmap`, creating it (and the directory) if missing.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir)?;
        let path: PathBuf = dir.join("blobmap");

        let existing = match std::fs::read(&path) {
            Ok(data) => Some(data),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };

        let Some(data) = existing else {
            let mut file = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&path)?;
            file.write_all(&file_header())?;
            file.sync_all()?;
            fsync_dir(dir)?;
            return Ok(Self {
                file,
                map: HashMap::new(),
                len: HEADER_LEN as u64,
            });
        };

        // a crash between create and the header fsync can leave a short
        // file; anything that is not a header prefix is foreign
        if data.len() < HEADER_LEN {
            if !file_header().starts_with(&data) {
                return Err(StoreError::Format("bad blobmap header"));
            }
        } else {
            if data[..4] != MAGIC {
                return Err(StoreError::Format("bad blobmap header"));
            }
            if data[4] != VERSION {
                return Err(StoreError::Format("unsupported blobmap version"));
            }
        }

        let mut map = HashMap::new();
        let mut at = HEADER_LEN.min(data.len());
        let mut valid_len = at as u64;
        while let Some(rec) = data.get(at..at + REC_LEN) {
            if checksum(&rec[..CHECKED_LEN]) != rec[CHECKED_LEN..] {
                if at + REC_LEN == data.len() {
                    break; // torn final record: truncate below
                }
                return Err(StoreError::Format("blobmap record corrupt"));
            }
            let mut blob = [0u8; 32];
            blob.copy_from_slice(&rec[..32]);
            let mut root = [0u8; 32];
            root.copy_from_slice(&rec[32..64]);
            let total_len = u64::from_le_bytes(rec[64..72].try_into().unwrap());
            map.insert(BlobId(blob), (ChunkId(root), total_len));
            at += REC_LEN;
            valid_len = at as u64;
        }

        let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
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
        let len = valid_len.max(HEADER_LEN as u64);
        Ok(Self { file, map, len })
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
        let check = checksum(&rec[..CHECKED_LEN]);
        rec[CHECKED_LEN..].copy_from_slice(&check);
        self.file.write_all(&rec)?;
        self.map.insert(blob, (root, total_len));
        self.len += REC_LEN as u64;
        Ok(())
    }

    /// Folds in records another writer appended since our last read, extending
    /// the in-memory map. Run under the odb write lock. A torn tail left by a
    /// crashed writer is truncated (we hold the lock, so none is mid-append).
    pub fn sync_from_disk(&mut self) -> Result<(), StoreError> {
        let size = self.file.metadata()?.len();
        if size <= self.len {
            return Ok(());
        }
        let mut tail = vec![0u8; (size - self.len) as usize];
        self.file.seek(SeekFrom::Start(self.len))?;
        self.file.read_exact(&mut tail)?;

        let mut at = 0usize;
        while let Some(rec) = tail.get(at..at + REC_LEN) {
            if checksum(&rec[..CHECKED_LEN]) != rec[CHECKED_LEN..] {
                break; // torn final record (we hold the lock): truncate below
            }
            let mut blob = [0u8; 32];
            blob.copy_from_slice(&rec[..32]);
            let mut root = [0u8; 32];
            root.copy_from_slice(&rec[32..64]);
            let total_len = u64::from_le_bytes(rec[64..72].try_into().unwrap());
            self.map.insert(BlobId(blob), (ChunkId(root), total_len));
            at += REC_LEN;
        }
        let new_len = self.len + at as u64;
        if (at as u64) < tail.len() as u64 {
            self.file.set_len(new_len)?; // drop a crashed writer's torn record
            self.file.sync_all()?;
            self.file.seek(SeekFrom::Start(new_len))?;
        }
        self.len = new_len;
        Ok(())
    }

    pub fn get(&self, blob: BlobId) -> Option<(ChunkId, u64)> {
        self.map.get(&blob).copied()
    }

    pub fn contains(&self, blob: BlobId) -> bool {
        self.map.contains_key(&blob)
    }

    pub fn sync(&mut self) -> Result<(), StoreError> {
        if !crate::relaxed_durability() {
            self.file.sync_all()?;
        }
        Ok(())
    }

    /// Raw fsync (no relaxed gate) — called by the group commit layer.
    pub fn fsync(&self) -> Result<(), StoreError> {
        self.file.sync_all()?;
        Ok(())
    }

    /// Bytes we have appended (our write cursor).
    pub fn appended_len(&self) -> u64 {
        self.len
    }

    /// The blobmap's true on-disk size.
    pub fn file_len(&self) -> Result<u64, StoreError> {
        Ok(self.file.metadata()?.len())
    }
}

impl Drop for BlobMap {
    fn drop(&mut self) {
        // best-effort durability; explicit sync() is the checked path
        let _ = self.file.sync_all();
    }
}
