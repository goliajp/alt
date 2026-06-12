//! alt chunk store: append-only altpack files holding BLAKE3-addressed,
//! zstd-compressed chunks.
//!
//! Business-agnostic stone: knows the altpack/altidx on-disk formats and
//! nothing about blobs, manifests, or git.
//!
//! On-disk layout (all integers little-endian):
//!
//! - `pack-<seq>.altpack` — `ALTP` magic + version byte, then records:
//!   `[blake3 32B][encoding u8][orig_len u32][stored_len u32][payload]`.
//!   Encodings: 0 = raw, 1 = zstd; 2 (delta) and 3 (parts) are reserved.
//! - `pack-<seq>.altidx` — index written when a pack is sealed: `ALTI`
//!   magic, version, count u64, then `[blake3 32B][offset u64]` sorted by
//!   id. The idx is a cache, never the truth: it is rebuilt from the pack
//!   when missing or unreadable.
//!
//! The highest-numbered pack is active (appendable); its index is recovered
//! on open by scanning the file and truncating any incomplete tail record
//! left by a crash. Reads decode the payload and re-hash it — a chunk that
//! does not hash to its own address is reported as corrupt, never returned.

mod idx;
mod pack;

use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use memmap2::Mmap;

use pack::{ENC_RAW, ENC_ZSTD, REC_HEADER_LEN, RecordHeader};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io")]
    Io(#[from] std::io::Error),
    #[error("store format: {0}")]
    Format(&'static str),
    #[error("chunk {0} not found")]
    NotFound(ChunkId),
    #[error("chunk {id} corrupt: {reason}")]
    Corrupt { id: ChunkId, reason: &'static str },
    #[error("chunk too large: {0} bytes")]
    TooLarge(usize),
}

/// BLAKE3 hash of the chunk's original (uncompressed) bytes — the chunk's
/// address, independent of how it is encoded on disk.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkId(pub [u8; 32]);

impl ChunkId {
    pub fn of(data: &[u8]) -> Self {
        Self(*blake3::hash(data).as_bytes())
    }
}

impl fmt::Display for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChunkId({self})")
    }
}

/// How a record is stored on disk (reserved encodings never reach callers:
/// reading one is a format error until the milestone that defines it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Raw,
    Zstd,
}

/// On-disk accounting for one chunk, for dedup/volume bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkStat {
    pub encoding: Encoding,
    pub orig_len: u32,
    pub stored_len: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Roll to a fresh pack once the active one reaches this many bytes.
    pub seal_threshold: u64,
    /// zstd compression level for chunk payloads.
    pub zstd_level: i32,
    /// Below this size compression cannot win; store raw.
    pub raw_threshold: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            seal_threshold: 1 << 30,
            zstd_level: 3,
            raw_threshold: 64,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Location {
    seq: u32,
    offset: u64,
}

struct Sealed {
    map: Mmap,
}

struct Active {
    seq: u32,
    /// Cursor sits at `len`; appends only ever move it forward.
    write: File,
    /// Separate handle for positioned reads so the cursor stays the writer's.
    read: File,
    len: u64,
    /// Records appended to this pack, in file order — becomes the idx at seal.
    entries: Vec<(ChunkId, u64)>,
}

/// Content-addressed chunk storage over a directory of altpack files.
pub struct ChunkStore {
    dir: PathBuf,
    opts: Options,
    sealed: HashMap<u32, Sealed>,
    active: Active,
    index: HashMap<ChunkId, Location>,
}

fn pack_path(dir: &Path, seq: u32) -> PathBuf {
    dir.join(format!("pack-{seq:08}.altpack"))
}

fn idx_path(dir: &Path, seq: u32) -> PathBuf {
    dir.join(format!("pack-{seq:08}.altidx"))
}

fn list_pack_seqs(dir: &Path) -> Result<Vec<u32>, StoreError> {
    let mut seqs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(seq) = name
            .strip_prefix("pack-")
            .and_then(|s| s.strip_suffix(".altpack"))
            .and_then(|s| s.parse::<u32>().ok())
        {
            seqs.push(seq);
        }
    }
    seqs.sort_unstable();
    Ok(seqs)
}

/// Opens (or creates) the active pack: validates the header, scans for the
/// valid record prefix, and truncates any torn tail so the writer starts at
/// a clean offset.
fn open_active(path: &Path, create: bool) -> Result<Active, StoreError> {
    if create {
        let mut write = OpenOptions::new().create_new(true).write(true).open(path)?;
        write.write_all(&pack::file_header())?;
        write.sync_all()?;
        let read = File::open(path)?;
        return Ok(Active {
            seq: 0, // caller fills in
            write,
            read,
            len: pack::HEADER_LEN as u64,
            entries: Vec::new(),
        });
    }

    let read = File::open(path)?;
    let size = read.metadata()?.len();
    let (recs, valid_len) = if size == 0 {
        (Vec::new(), 0)
    } else {
        let map = unsafe { Mmap::map(&read)? };
        if (size as usize) < pack::HEADER_LEN {
            // a crash between create and the header fsync can leave a short
            // file; anything that is not a header prefix is foreign
            if pack::file_header().starts_with(&map[..]) {
                (Vec::new(), 0)
            } else {
                return Err(StoreError::Format("bad altpack header"));
            }
        } else {
            pack::scan(&map)?
        }
    };

    let mut write = OpenOptions::new().write(true).open(path)?;
    if valid_len == 0 {
        write.set_len(0)?;
        write.write_all(&pack::file_header())?;
        write.sync_all()?;
        return Ok(Active {
            seq: 0,
            write,
            read,
            len: pack::HEADER_LEN as u64,
            entries: Vec::new(),
        });
    }
    if valid_len < size {
        write.set_len(valid_len)?;
        write.sync_all()?;
    }
    write.seek(SeekFrom::Start(valid_len))?;
    let entries = recs.iter().map(|(hdr, off)| (hdr.id, *off)).collect();
    Ok(Active {
        seq: 0,
        write,
        read,
        len: valid_len,
        entries,
    })
}

impl ChunkStore {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        Self::open_with(dir, Options::default())
    }

    pub fn open_with(dir: impl Into<PathBuf>, opts: Options) -> Result<Self, StoreError> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        let seqs = list_pack_seqs(&dir)?;

        let mut sealed = HashMap::new();
        let mut index = HashMap::new();

        let (&active_seq, sealed_seqs) = match seqs.split_last() {
            Some((last, rest)) => (last, rest),
            None => {
                let mut active = open_active(&pack_path(&dir, 1), true)?;
                active.seq = 1;
                pack::fsync_dir(&dir)?;
                return Ok(Self {
                    dir,
                    opts,
                    sealed,
                    active,
                    index,
                });
            }
        };

        for &seq in sealed_seqs {
            let file = File::open(pack_path(&dir, seq))?;
            let map = unsafe { Mmap::map(&file)? };
            pack::check_file_header(&map)?;
            let entries = match idx::read(&idx_path(&dir, seq)) {
                Ok(entries) => entries,
                Err(_) => {
                    // idx is a cache: rebuild from the pack and repair it
                    let (recs, valid_len) = pack::scan(&map)?;
                    if valid_len != map.len() as u64 {
                        return Err(StoreError::Format("sealed pack truncated"));
                    }
                    let entries: Vec<_> = recs.iter().map(|(hdr, off)| (hdr.id, *off)).collect();
                    idx::write(&idx_path(&dir, seq), &entries)?;
                    pack::fsync_dir(&dir)?;
                    entries
                }
            };
            for (id, offset) in entries {
                index.insert(id, Location { seq, offset });
            }
            sealed.insert(seq, Sealed { map });
        }

        let mut active = open_active(&pack_path(&dir, active_seq), false)?;
        active.seq = active_seq;
        for (id, offset) in &active.entries {
            index.insert(
                *id,
                Location {
                    seq: active_seq,
                    offset: *offset,
                },
            );
        }

        Ok(Self {
            dir,
            opts,
            sealed,
            active,
            index,
        })
    }

    /// Stores `data`, deduplicating by content: a chunk already present is
    /// not written again. Returns the chunk's address either way.
    pub fn put(&mut self, data: &[u8]) -> Result<ChunkId, StoreError> {
        let id = ChunkId::of(data);
        if self.index.contains_key(&id) {
            return Ok(id);
        }
        let orig_len = u32::try_from(data.len()).map_err(|_| StoreError::TooLarge(data.len()))?;

        let compressed = if data.len() < self.opts.raw_threshold {
            None
        } else {
            Some(zstd::encode_all(data, self.opts.zstd_level)?)
        };
        let (encoding, payload): (u8, &[u8]) = match &compressed {
            Some(z) if z.len() < data.len() => (ENC_ZSTD, z),
            _ => (ENC_RAW, data),
        };

        let header = RecordHeader {
            id,
            encoding,
            orig_len,
            stored_len: payload.len() as u32,
        };
        let offset = self.active.len;
        if let Err(e) = self.append(&header.encode(), payload) {
            // chop any torn bytes so the next append starts at a clean offset
            self.active.write.set_len(self.active.len)?;
            self.active.write.seek(SeekFrom::Start(self.active.len))?;
            return Err(e.into());
        }
        self.active.len += (REC_HEADER_LEN + payload.len()) as u64;
        self.active.entries.push((id, offset));
        self.index.insert(
            id,
            Location {
                seq: self.active.seq,
                offset,
            },
        );

        if self.active.len >= self.opts.seal_threshold {
            self.seal_and_roll()?;
        }
        Ok(id)
    }

    fn append(&mut self, header: &[u8], payload: &[u8]) -> std::io::Result<()> {
        self.active.write.write_all(header)?;
        self.active.write.write_all(payload)
    }

    /// Fsyncs the active pack, writes its idx, and starts a fresh pack.
    fn seal_and_roll(&mut self) -> Result<(), StoreError> {
        self.active.write.sync_all()?;
        idx::write(&idx_path(&self.dir, self.active.seq), &self.active.entries)?;
        pack::fsync_dir(&self.dir)?;

        let map = unsafe { Mmap::map(&self.active.read)? };
        self.sealed.insert(self.active.seq, Sealed { map });

        let seq = self.active.seq + 1;
        let mut active = open_active(&pack_path(&self.dir, seq), true)?;
        active.seq = seq;
        pack::fsync_dir(&self.dir)?;
        self.active = active;
        Ok(())
    }

    /// Reads a chunk back. The payload is decoded and re-hashed: a result is
    /// only ever the exact bytes the address was computed from.
    pub fn get(&self, id: ChunkId) -> Result<Vec<u8>, StoreError> {
        let loc = *self.index.get(&id).ok_or(StoreError::NotFound(id))?;
        let (header, payload) = self.read_record(id, loc)?;
        if header.id != id {
            return Err(StoreError::Corrupt {
                id,
                reason: "record id mismatch",
            });
        }
        let data = match header.encoding {
            ENC_RAW => payload,
            ENC_ZSTD => zstd::decode_all(&payload[..]).map_err(|_| StoreError::Corrupt {
                id,
                reason: "zstd decode failed",
            })?,
            _ => return Err(StoreError::Format("reserved record encoding")),
        };
        if data.len() != header.orig_len as usize {
            return Err(StoreError::Corrupt {
                id,
                reason: "length mismatch",
            });
        }
        if ChunkId::of(&data) != id {
            return Err(StoreError::Corrupt {
                id,
                reason: "hash mismatch",
            });
        }
        Ok(data)
    }

    pub fn contains(&self, id: ChunkId) -> bool {
        self.index.contains_key(&id)
    }

    /// On-disk accounting for one chunk (encoding and stored size).
    pub fn stat(&self, id: ChunkId) -> Result<ChunkStat, StoreError> {
        let loc = *self.index.get(&id).ok_or(StoreError::NotFound(id))?;
        let header = self.read_header(id, loc)?;
        let encoding = match header.encoding {
            ENC_RAW => Encoding::Raw,
            ENC_ZSTD => Encoding::Zstd,
            _ => return Err(StoreError::Format("reserved record encoding")),
        };
        Ok(ChunkStat {
            encoding,
            orig_len: header.orig_len,
            stored_len: header.stored_len,
        })
    }

    /// Number of distinct chunks stored.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Fsyncs the active pack — the durability point between seals.
    pub fn flush(&mut self) -> Result<(), StoreError> {
        self.active.write.sync_all()?;
        Ok(())
    }

    fn read_header(&self, id: ChunkId, loc: Location) -> Result<RecordHeader, StoreError> {
        if loc.seq == self.active.seq {
            let mut buf = [0u8; REC_HEADER_LEN];
            pack::read_exact_at(&self.active.read, &mut buf, loc.offset)?;
            return Ok(RecordHeader::parse(&buf));
        }
        let sealed = self
            .sealed
            .get(&loc.seq)
            .ok_or(StoreError::Format("unknown pack"))?;
        let at = loc.offset as usize;
        let buf = sealed
            .map
            .get(at..at + REC_HEADER_LEN)
            .ok_or(StoreError::Corrupt {
                id,
                reason: "record out of bounds",
            })?;
        Ok(RecordHeader::parse(buf.try_into().unwrap()))
    }

    fn read_record(
        &self,
        id: ChunkId,
        loc: Location,
    ) -> Result<(RecordHeader, Vec<u8>), StoreError> {
        let header = self.read_header(id, loc)?;
        let at = loc.offset + REC_HEADER_LEN as u64;
        if loc.seq == self.active.seq {
            let mut payload = vec![0u8; header.stored_len as usize];
            pack::read_exact_at(&self.active.read, &mut payload, at)?;
            return Ok((header, payload));
        }
        let sealed = &self.sealed[&loc.seq];
        let at = at as usize;
        let payload =
            sealed
                .map
                .get(at..at + header.stored_len as usize)
                .ok_or(StoreError::Corrupt {
                    id,
                    reason: "record out of bounds",
                })?;
        Ok((header, payload.to_vec()))
    }
}

impl Drop for ChunkStore {
    fn drop(&mut self) {
        // best-effort durability; explicit flush() is the checked path
        let _ = self.active.write.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random bytes (splitmix64 stream).
    fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut state = seed;
        while out.len() < len {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            out.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
        }
        out.truncate(len);
        out
    }

    #[test]
    fn put_get_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ChunkStore::open(dir.path()).unwrap();
        let cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"hello".to_vec(),
            vec![0u8; 100_000],
            random_bytes(1 << 20, 1),
        ];
        let ids: Vec<ChunkId> = cases.iter().map(|c| store.put(c).unwrap()).collect();
        for (case, id) in cases.iter().zip(&ids) {
            assert_eq!(&store.get(*id).unwrap(), case);
            let stat = store.stat(*id).unwrap();
            assert_eq!(stat.orig_len as usize, case.len());
        }
        assert_eq!(store.len(), cases.len());
    }

    #[test]
    fn dedup_stores_identical_content_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ChunkStore::open(dir.path()).unwrap();
        let data = random_bytes(10_000, 2);
        let a = store.put(&data).unwrap();
        let size_after_first = store.active.len;
        let b = store.put(&data).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            store.active.len, size_after_first,
            "second put must not write"
        );
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn encoding_choices() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ChunkStore::open(dir.path()).unwrap();

        // below the raw threshold: stored raw without trying zstd
        let tiny = store.put(b"tiny").unwrap();
        assert_eq!(store.stat(tiny).unwrap().encoding, Encoding::Raw);

        // compressible: zstd wins
        let zeros = store.put(&vec![0u8; 100_000]).unwrap();
        let stat = store.stat(zeros).unwrap();
        assert_eq!(stat.encoding, Encoding::Zstd);
        assert!(stat.stored_len < stat.orig_len);

        // incompressible: zstd would grow it, falls back to raw
        let noise = store.put(&random_bytes(4096, 3)).unwrap();
        let stat = store.stat(noise).unwrap();
        assert_eq!(stat.encoding, Encoding::Raw);
        assert_eq!(stat.stored_len, stat.orig_len);
    }

    #[test]
    fn missing_chunk_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChunkStore::open(dir.path()).unwrap();
        let err = store.get(ChunkId([0u8; 32])).unwrap_err();
        assert!(matches!(err, StoreError::NotFound(_)));
    }
}
