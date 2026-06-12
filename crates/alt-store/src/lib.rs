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

mod blob;
mod blobmap;
mod delta;
mod idx;
mod pack;

pub use blob::{BlobOptions, BlobStore};

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use memmap2::Mmap;

use pack::{ENC_DELTA, ENC_RAW, ENC_ZSTD, REC_HEADER_LEN, RecordHeader};

/// Write-side cap on lineage-delta chain length. Later re-encoding of a
/// base can stretch an existing chain past this — the cap is a cost
/// bound, not a correctness bound. Reads resolve chains of any length
/// iteratively and reject only true cycles, which can come solely from
/// on-disk corruption: the writer provably never closes a loop.
const MAX_DELTA_DEPTH: usize = 16;
/// Budget for the delta-base cache (resolved bases kept hot).
const DELTA_CACHE_BUDGET: usize = 64 << 20;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io")]
    Io(#[from] std::io::Error),
    #[error("store format: {0}")]
    Format(&'static str),
    #[error("chunk {0} not found")]
    NotFound(ChunkId),
    #[error("blob {0} not found")]
    BlobNotFound(BlobId),
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

/// BLAKE3 hash of a blob's full content — the blob's address, independent
/// of how it was chunked, so retuning CDC parameters never moves blobs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlobId(pub [u8; 32]);

/// Above this size the rayon-parallel hasher wins; below it, plain BLAKE3.
const RAYON_HASH_THRESHOLD: usize = 128 * 1024;

impl BlobId {
    pub fn of(data: &[u8]) -> Self {
        if data.len() >= RAYON_HASH_THRESHOLD {
            Self(
                *blake3::Hasher::new()
                    .update_rayon(data)
                    .finalize()
                    .as_bytes(),
            )
        } else {
            Self(*blake3::hash(data).as_bytes())
        }
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobId({self})")
    }
}

/// Session-scoped dedup/volume accounting (not persisted).
#[derive(Debug, Default, Clone, Copy)]
pub struct Counters {
    /// `put` calls.
    pub puts: u64,
    /// `put` calls answered by an already-stored chunk.
    pub dedup_hits: u64,
    /// Original bytes offered to `put`, including dedup hits.
    pub bytes_in: u64,
    /// Bytes appended to packs (record headers + payloads).
    pub bytes_written: u64,
    /// Chunks re-encoded as lineage deltas.
    pub lineage_deltas: u64,
    /// Bytes of records superseded by a smaller delta re-encoding
    /// (reclaimable by compaction; still on disk until then).
    pub bytes_superseded: u64,
}

/// How a record is stored on disk (reserved encodings never reach callers:
/// reading one is a format error until the milestone that defines it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Raw,
    Zstd,
    /// zstd against a lineage base (patch-from); the payload names the
    /// base chunk in its first 32 bytes.
    Delta,
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

/// Bounded, insertion-order cache of resolved delta bases.
struct DeltaCache {
    map: HashMap<ChunkId, Arc<Vec<u8>>>,
    order: VecDeque<ChunkId>,
    bytes: usize,
}

impl DeltaCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
        }
    }

    fn get(&self, id: &ChunkId) -> Option<Arc<Vec<u8>>> {
        self.map.get(id).cloned()
    }

    fn put(&mut self, id: ChunkId, data: Arc<Vec<u8>>) {
        if self.map.contains_key(&id) {
            return;
        }
        self.bytes += data.len();
        self.map.insert(id, data);
        self.order.push_back(id);
        while self.bytes > DELTA_CACHE_BUDGET {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(data) = self.map.remove(&old) {
                self.bytes -= data.len();
            }
        }
    }
}

/// Content-addressed chunk storage over a directory of altpack files.
pub struct ChunkStore {
    dir: PathBuf,
    opts: Options,
    sealed: HashMap<u32, Sealed>,
    active: Active,
    index: HashMap<ChunkId, Location>,
    counters: Counters,
    cache: Mutex<DeltaCache>,
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
                    counters: Counters::default(),
                    cache: Mutex::new(DeltaCache::new()),
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
            counters: Counters::default(),
            cache: Mutex::new(DeltaCache::new()),
        })
    }

    /// Stores `data`, deduplicating by content: a chunk already present is
    /// not written again. Returns the chunk's address either way.
    pub fn put(&mut self, data: &[u8]) -> Result<ChunkId, StoreError> {
        let id = ChunkId::of(data);
        self.counters.puts += 1;
        self.counters.bytes_in += data.len() as u64;
        if self.index.contains_key(&id) {
            self.counters.dedup_hits += 1;
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
        self.append_record(&header, payload)?;
        Ok(id)
    }

    /// Re-encodes an existing chunk as a lineage delta against `base`.
    /// Returns false (and writes nothing) when the delta does not win:
    /// already delta-encoded, chain too deep, would form a cycle, or not
    /// actually smaller. The chunk's address never changes — only its
    /// storage form (identity/encoding decoupling).
    pub fn reencode_as_delta(&mut self, id: ChunkId, base: ChunkId) -> Result<bool, StoreError> {
        if id == base {
            return Ok(false);
        }
        let id_loc = *self.index.get(&id).ok_or(StoreError::NotFound(id))?;
        if !self.index.contains_key(&base) {
            return Err(StoreError::NotFound(base));
        }
        let current = self.read_header(id, id_loc)?;
        if current.encoding == ENC_DELTA {
            return Ok(false); // one re-encoding per chunk; also breaks cycles
        }

        // walk the base's chain: depth cap and cycle guard in one pass
        let mut node = base;
        let mut hops = 1usize;
        loop {
            if node == id {
                return Ok(false); // would close a loop through id
            }
            let loc = *self.index.get(&node).ok_or(StoreError::NotFound(node))?;
            let header = self.read_header(node, loc)?;
            if header.encoding != ENC_DELTA {
                break;
            }
            hops += 1;
            if hops > MAX_DELTA_DEPTH {
                return Ok(false);
            }
            node = self.read_base_id(node, loc)?;
        }

        let data = self.get(id)?;
        let base_data = self.get(base)?;
        let Some(z) = delta::compress_with_base(&data, &base_data, self.opts.zstd_level) else {
            return Ok(false);
        };
        let stored = 32 + z.len();
        if stored as u64 >= current.stored_len as u64 {
            return Ok(false); // the delta must actually win
        }

        let mut payload = Vec::with_capacity(stored);
        payload.extend_from_slice(&base.0);
        payload.extend_from_slice(&z);
        let header = RecordHeader {
            id,
            encoding: ENC_DELTA,
            orig_len: current.orig_len,
            stored_len: stored as u32,
        };
        self.append_record(&header, &payload)?;
        self.counters.lineage_deltas += 1;
        self.counters.bytes_superseded += (REC_HEADER_LEN as u64) + current.stored_len as u64;
        Ok(true)
    }

    /// Appends one record and points the index at it (a re-encode
    /// supersedes the previous location).
    fn append_record(&mut self, header: &RecordHeader, payload: &[u8]) -> Result<(), StoreError> {
        let offset = self.active.len;
        if let Err(e) = self.append(&header.encode(), payload) {
            // chop any torn bytes so the next append starts at a clean offset
            self.active.write.set_len(self.active.len)?;
            self.active.write.seek(SeekFrom::Start(self.active.len))?;
            return Err(e.into());
        }
        self.active.len += (REC_HEADER_LEN + payload.len()) as u64;
        self.counters.bytes_written += (REC_HEADER_LEN + payload.len()) as u64;
        self.active.entries.push((header.id, offset));
        self.index.insert(
            header.id,
            Location {
                seq: self.active.seq,
                offset,
            },
        );
        if self.active.len >= self.opts.seal_threshold {
            self.seal_and_roll()?;
        }
        Ok(())
    }

    fn append(&mut self, header: &[u8], payload: &[u8]) -> std::io::Result<()> {
        self.active.write.write_all(header)?;
        self.active.write.write_all(payload)
    }

    /// The base named by a delta record (its payload's first 32 bytes).
    fn read_base_id(&self, id: ChunkId, loc: Location) -> Result<ChunkId, StoreError> {
        if loc.seq == self.active.seq {
            let mut buf = [0u8; 32];
            pack::read_exact_at(
                &self.active.read,
                &mut buf,
                loc.offset + REC_HEADER_LEN as u64,
            )?;
            return Ok(ChunkId(buf));
        }
        let sealed = self
            .sealed
            .get(&loc.seq)
            .ok_or(StoreError::Format("unknown pack"))?;
        let at = loc.offset as usize + REC_HEADER_LEN;
        let buf = sealed.map.get(at..at + 32).ok_or(StoreError::Corrupt {
            id,
            reason: "record out of bounds",
        })?;
        Ok(ChunkId(buf.try_into().unwrap()))
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

    /// Reads a chunk back. The payload is decoded (resolving any lineage
    /// delta chain) and re-hashed level by level: a result is only ever
    /// the exact bytes the address was computed from.
    pub fn get(&self, id: ChunkId) -> Result<Vec<u8>, StoreError> {
        // descend: collect delta frames until a full record or a cached
        // base; iterative so chain length never threatens the stack
        let mut frames: Vec<(ChunkId, u32, Vec<u8>)> = Vec::new();
        let mut cur = id;
        let data: Vec<u8>;
        loop {
            if !frames.is_empty()
                && let Some(hit) = self.cache.lock().unwrap().get(&cur)
            {
                data = (*hit).clone();
                break;
            }
            if frames.iter().any(|(fid, ..)| *fid == cur) {
                // only reachable through on-disk corruption of a base id:
                // the writer never closes a loop
                return Err(StoreError::Corrupt {
                    id: cur,
                    reason: "delta chain cycle",
                });
            }
            let loc = *self.index.get(&cur).ok_or(StoreError::NotFound(cur))?;
            let (header, payload) = self.read_record(cur, loc)?;
            if header.id != cur {
                return Err(StoreError::Corrupt {
                    id: cur,
                    reason: "record id mismatch",
                });
            }
            match header.encoding {
                ENC_RAW => {
                    data = Self::verified(cur, payload, header.orig_len)?;
                    break;
                }
                ENC_ZSTD => {
                    let raw = zstd::decode_all(&payload[..]).map_err(|_| StoreError::Corrupt {
                        id: cur,
                        reason: "zstd decode failed",
                    })?;
                    data = Self::verified(cur, raw, header.orig_len)?;
                    break;
                }
                ENC_DELTA => {
                    let base_bytes = payload.get(..32).ok_or(StoreError::Corrupt {
                        id: cur,
                        reason: "delta payload too short",
                    })?;
                    let base_id = ChunkId(base_bytes.try_into().unwrap());
                    frames.push((cur, header.orig_len, payload[32..].to_vec()));
                    cur = base_id;
                }
                _ => return Err(StoreError::Format("reserved record encoding")),
            }
        }

        // unwind: apply frames top-down, keeping every base hot
        let mut data = data;
        let mut base_id = cur;
        while let Some((fid, orig_len, z)) = frames.pop() {
            let out = delta::decompress_with_base(&z, &data, orig_len as usize).ok_or(
                StoreError::Corrupt {
                    id: fid,
                    reason: "delta decode failed",
                },
            )?;
            let out = Self::verified(fid, out, orig_len)?;
            self.cache.lock().unwrap().put(base_id, Arc::new(data));
            data = out;
            base_id = fid;
        }
        Ok(data)
    }

    fn verified(id: ChunkId, data: Vec<u8>, orig_len: u32) -> Result<Vec<u8>, StoreError> {
        if data.len() != orig_len as usize {
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
            ENC_DELTA => Encoding::Delta,
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

    pub fn counters(&self) -> Counters {
        self.counters
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

    /// `n` similar versions of a small "file": shared body, a version
    /// marker in the middle.
    fn versions(n: usize, seed: u64) -> Vec<Vec<u8>> {
        let body = random_bytes(4096, seed);
        (0..n)
            .map(|i| {
                let mut v = body.clone();
                v[2000..2008].copy_from_slice(&(i as u64).to_le_bytes());
                v
            })
            .collect()
    }

    #[test]
    fn lineage_delta_reencodes_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ChunkStore::open(dir.path()).unwrap();
        let vs = versions(2, 10);
        let old = store.put(&vs[0]).unwrap();
        let new = store.put(&vs[1]).unwrap();

        assert!(store.reencode_as_delta(old, new).unwrap());
        assert_eq!(store.stat(old).unwrap().encoding, Encoding::Delta);
        assert!(
            store.stat(old).unwrap().stored_len < 200,
            "near-identical versions must delta to almost nothing"
        );
        assert_eq!(
            store.get(old).unwrap(),
            vs[0],
            "address unchanged, bytes unchanged"
        );
        assert_eq!(store.get(new).unwrap(), vs[1]);
        assert_eq!(store.counters().lineage_deltas, 1);
        assert!(store.counters().bytes_superseded > 0);

        // one re-encoding per chunk
        assert!(!store.reencode_as_delta(old, new).unwrap());
    }

    #[test]
    fn lineage_delta_refuses_cycles_and_useless_deltas() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ChunkStore::open(dir.path()).unwrap();
        let vs = versions(2, 11);
        let a = store.put(&vs[0]).unwrap();
        let b = store.put(&vs[1]).unwrap();
        let noise = store.put(&random_bytes(4096, 99)).unwrap();

        assert!(store.reencode_as_delta(a, b).unwrap());
        // b against a would close a loop through a's base
        assert!(!store.reencode_as_delta(b, a).unwrap());
        // unrelated content: the delta cannot win
        assert!(!store.reencode_as_delta(noise, b).unwrap());
        assert_eq!(store.get(noise).unwrap().len(), 4096);
        // self and missing bases
        assert!(!store.reencode_as_delta(a, a).unwrap());
        assert!(matches!(
            store.reencode_as_delta(b, ChunkId([9u8; 32])),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn lineage_delta_chains_respect_the_depth_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ChunkStore::open(dir.path()).unwrap();
        let vs = versions(MAX_DELTA_DEPTH + 5, 12);
        let ids: Vec<ChunkId> = vs.iter().map(|v| store.put(v).unwrap()).collect();

        // newest stays full; walk back re-encoding old against newer
        let mut encoded = 0usize;
        let mut refused = 0usize;
        for i in (0..ids.len() - 1).rev() {
            if store.reencode_as_delta(ids[i], ids[i + 1]).unwrap() {
                encoded += 1;
            } else {
                refused += 1;
            }
        }
        assert!(encoded >= MAX_DELTA_DEPTH - 1);
        assert!(refused >= 1, "the cap must refuse the deepest links");
        for (v, id) in vs.iter().zip(&ids) {
            assert_eq!(&store.get(*id).unwrap(), v, "{id}");
        }
    }
}
