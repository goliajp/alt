//! `map.alt`: the bidirectional sha1/sha256 ↔ blake3 ↔ (kind, size) index —
//! the only bridge between the git-facing logical layer and the physical
//! store. It is truth, not cache: an object's kind (and thus its canonical
//! header) is not derivable from stored content alone, so records carry
//! their own checksum. A torn tail (crash mid-append) is truncated on open;
//! a bad record anywhere else is corruption and reported as such.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_store::BlobId;

use crate::OdbError;

const MAGIC: [u8; 4] = *b"ALTG";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 5;
/// Record: algo u8 + git oid 32 (sha1 zero-padded) + blake3 32 + kind u8 +
/// size u64 + checksum 8. All integers little-endian.
const REC_LEN: usize = 82;
const CHECKED_LEN: usize = REC_LEN - 8;

fn file_header() -> [u8; HEADER_LEN] {
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION]
}

fn checksum(checked: &[u8]) -> [u8; 8] {
    blake3::hash(checked).as_bytes()[..8].try_into().unwrap()
}

fn algo_byte(algo: HashAlgo) -> u8 {
    match algo {
        HashAlgo::Sha1 => 1,
        HashAlgo::Sha256 => 2,
    }
}

fn kind_byte(kind: ObjectKind) -> u8 {
    match kind {
        ObjectKind::Blob => 0,
        ObjectKind::Tree => 1,
        ObjectKind::Commit => 2,
        ObjectKind::Tag => 3,
    }
}

fn parse_kind(byte: u8) -> Result<ObjectKind, OdbError> {
    Ok(match byte {
        0 => ObjectKind::Blob,
        1 => ObjectKind::Tree,
        2 => ObjectKind::Commit,
        3 => ObjectKind::Tag,
        _ => return Err(OdbError::Format("bad object kind in map.alt")),
    })
}

/// One object's identities: its git oid, its content's blake3 address, and
/// the kind/size needed to rebuild the canonical `"<kind> <size>\0"` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapEntry {
    pub git: ObjectId,
    pub alt: BlobId,
    pub kind: ObjectKind,
    pub size: u64,
}

impl MapEntry {
    fn encode(&self) -> [u8; REC_LEN] {
        let mut rec = [0u8; REC_LEN];
        rec[0] = algo_byte(self.git.algo());
        rec[1..1 + self.git.as_bytes().len()].copy_from_slice(self.git.as_bytes());
        rec[33..65].copy_from_slice(&self.alt.0);
        rec[65] = kind_byte(self.kind);
        rec[66..74].copy_from_slice(&self.size.to_le_bytes());
        let check = checksum(&rec[..CHECKED_LEN]);
        rec[CHECKED_LEN..].copy_from_slice(&check);
        rec
    }

    fn parse(rec: &[u8]) -> Result<Self, OdbError> {
        let algo = match rec[0] {
            1 => HashAlgo::Sha1,
            2 => HashAlgo::Sha256,
            _ => return Err(OdbError::Format("bad hash algo in map.alt")),
        };
        let git = ObjectId::from_bytes(algo, &rec[1..1 + algo.raw_len()])
            .map_err(|_| OdbError::Format("bad git oid in map.alt"))?;
        let mut alt = [0u8; 32];
        alt.copy_from_slice(&rec[33..65]);
        Ok(Self {
            git,
            alt: BlobId(alt),
            kind: parse_kind(rec[65])?,
            size: u64::from_le_bytes(rec[66..74].try_into().unwrap()),
        })
    }
}

pub struct ObjectMap {
    file: File,
    entries: Vec<MapEntry>,
    by_git: HashMap<ObjectId, u32>,
    /// One content can back several git objects (same bytes, different
    /// kind, or both hash algos), so the reverse edge is a list.
    by_alt: HashMap<BlobId, Vec<u32>>,
    /// Byte offset past the last record we have read — lets `sync_from_disk`
    /// fold in only what another writer appended since.
    len: u64,
}

impl ObjectMap {
    pub fn open(path: &Path) -> Result<Self, OdbError> {
        let existing = match std::fs::read(path) {
            Ok(data) => Some(data),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };

        let Some(data) = existing else {
            let mut file = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(path)?;
            file.write_all(&file_header())?;
            file.sync_all()?;
            return Ok(Self::empty(file));
        };

        // a crash between create and the header fsync can leave a short
        // file; anything that is not a header prefix is foreign
        if data.len() < HEADER_LEN {
            if !file_header().starts_with(&data) {
                return Err(OdbError::Format("bad map.alt header"));
            }
        } else {
            if data[..4] != MAGIC {
                return Err(OdbError::Format("bad map.alt header"));
            }
            if data[4] != VERSION {
                return Err(OdbError::Format("unsupported map.alt version"));
            }
        }

        let mut this = Self::empty(OpenOptions::new().read(true).write(true).open(path)?);
        let mut at = HEADER_LEN.min(data.len());
        let mut valid_len = at as u64;
        while let Some(rec) = data.get(at..at + REC_LEN) {
            if checksum(&rec[..CHECKED_LEN]) != rec[CHECKED_LEN..] {
                if at + REC_LEN == data.len() {
                    break; // torn final record: truncate below
                }
                return Err(OdbError::Format("map.alt record corrupt"));
            }
            this.insert(MapEntry::parse(rec)?);
            at += REC_LEN;
            valid_len = at as u64;
        }

        if valid_len < HEADER_LEN as u64 {
            this.file.set_len(0)?;
            this.file.write_all(&file_header())?;
            this.file.sync_all()?;
        } else {
            if valid_len < data.len() as u64 {
                this.file.set_len(valid_len)?;
                this.file.sync_all()?;
            }
            this.file.seek(SeekFrom::Start(valid_len))?;
        }
        this.len = valid_len.max(HEADER_LEN as u64);
        Ok(this)
    }

    fn empty(file: File) -> Self {
        Self {
            file,
            entries: Vec::new(),
            by_git: HashMap::new(),
            by_alt: HashMap::new(),
            len: HEADER_LEN as u64,
        }
    }

    fn insert(&mut self, entry: MapEntry) {
        let at = self.entries.len() as u32;
        self.entries.push(entry);
        self.by_git.insert(entry.git, at);
        self.by_alt.entry(entry.alt).or_default().push(at);
    }

    pub fn append(&mut self, entry: MapEntry) -> Result<(), OdbError> {
        self.file.write_all(&entry.encode())?;
        self.insert(entry);
        self.len += REC_LEN as u64;
        Ok(())
    }

    /// Folds in records another writer appended since our last read. Run under
    /// the odb write lock. A torn tail left by a crashed writer is truncated.
    pub fn sync_from_disk(&mut self) -> Result<(), OdbError> {
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
            self.insert(MapEntry::parse(rec)?);
            at += REC_LEN;
        }
        let new_len = self.len + at as u64;
        if (at as u64) < tail.len() as u64 {
            self.file.set_len(new_len)?;
            self.file.sync_all()?;
            self.file.seek(SeekFrom::Start(new_len))?;
        }
        self.len = new_len;
        Ok(())
    }

    pub fn by_git(&self, oid: &ObjectId) -> Option<&MapEntry> {
        self.by_git.get(oid).map(|&at| &self.entries[at as usize])
    }

    pub fn by_alt(&self, id: BlobId) -> impl Iterator<Item = &MapEntry> {
        self.by_alt
            .get(&id)
            .into_iter()
            .flatten()
            .map(|&at| &self.entries[at as usize])
    }

    /// All entries in append order.
    pub fn iter(&self) -> impl Iterator<Item = &MapEntry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn sync(&mut self) -> Result<(), OdbError> {
        if !alt_store::relaxed_durability() {
            self.file.sync_all()?;
        }
        Ok(())
    }

    /// Raw fsync (no relaxed gate) — called by the group commit layer.
    pub fn fsync(&self) -> Result<(), OdbError> {
        self.file.sync_all()?;
        Ok(())
    }

    /// An independent fd to the (stable, never-rewritten) `map.alt` file, for
    /// the daemon's off-write-path fsync.
    pub fn sync_handle(&self) -> Result<std::fs::File, OdbError> {
        Ok(self.file.try_clone()?)
    }

    /// Bytes we have appended (our write cursor).
    pub fn appended_len(&self) -> u64 {
        self.len
    }

    /// Truncates the map back to `target_len` (a previous `appended_len`),
    /// dropping every entry appended past that point from the in-memory
    /// indices. `target_len` must be at a record boundary and not below the
    /// file header. Fsyncs the truncation so it's durable on recovery.
    pub fn rewind(&mut self, target_len: u64) -> Result<(), OdbError> {
        if target_len > self.len {
            return Err(OdbError::Format("map.alt rewind target above cursor"));
        }
        if target_len < HEADER_LEN as u64 {
            return Err(OdbError::Format("map.alt rewind target below header"));
        }
        let drop_bytes = self.len - target_len;
        if !drop_bytes.is_multiple_of(REC_LEN as u64) {
            return Err(OdbError::Format("map.alt rewind target mid-record"));
        }
        let pop_count = (drop_bytes / REC_LEN as u64) as usize;
        let keep = self.entries.len() - pop_count;
        let dropped: Vec<MapEntry> = self.entries.drain(keep..).collect();
        let kept_at = keep as u32;
        for entry in dropped {
            self.by_git.remove(&entry.git);
            if let Some(slots) = self.by_alt.get_mut(&entry.alt) {
                slots.retain(|&at| at < kept_at);
                if slots.is_empty() {
                    self.by_alt.remove(&entry.alt);
                }
            }
        }
        self.file.set_len(target_len)?;
        self.file.sync_all()?;
        self.file.seek(SeekFrom::Start(target_len))?;
        self.len = target_len;
        Ok(())
    }

    /// The map's true on-disk size.
    pub fn file_len(&self) -> Result<u64, OdbError> {
        Ok(self.file.metadata()?.len())
    }
}

impl Drop for ObjectMap {
    fn drop(&mut self) {
        // best-effort durability; explicit sync() is the checked path
        let _ = self.file.sync_all();
    }
}
