//! alt chunk store: append-only altpack files holding BLAKE3-addressed,
//! zstd-compressed chunks.
//!
//! Pure-logic crate, business-agnostic: knows the altpack/altidx on-disk
//! formats and nothing about blobs, manifests, or git.
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
/// Lineage delta codec (zstd ref-prefix / patch-from). Public as a
/// primitive — reused by the store's encode/decode and benched directly.
pub mod delta;
mod idx;
mod pack;

pub use blob::{BlobOptions, BlobSink, BlobStore, StoreCheckpoint};

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
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

/// What one compaction did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactReport {
    pub packs_before: usize,
    pub packs_after: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// Live records carried into the compacted pack.
    pub records: usize,
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

/// How much a chunk read re-hashes. Tiered verification (M3.5 §阶段 B):
/// the per-layer/per-chunk hash was the dominant read cost, so the default
/// read verifies once at the boundary and deep scrubbing is opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verify {
    /// no hashing — the caller verifies at a higher boundary (the blob hash)
    None,
    /// hash only the requested address (the assembled result)
    Boundary,
    /// hash every layer (fsck: localize, not just detect)
    Deep,
}

impl Verify {
    /// Whether to hash a layer; `is_boundary` is true for the requested id.
    fn hash_at(self, is_boundary: bool) -> bool {
        matches!(self, Verify::Deep) || (matches!(self, Verify::Boundary) && is_boundary)
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

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.bytes = 0;
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

/// A point-in-time write cursor of a [`ChunkStore`], taken at the start of a
/// write batch so a later [`ChunkStore::rewind`] can roll back every record
/// appended after it. Captures the active pack's seq + appended length;
/// rewinding across a seal/roll is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkCheckpoint {
    pack_seq: u32,
    pack_len: u64,
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

/// Repro/diagnostic knob (env `ALT_RELAXED_DURABILITY`): skip per-commit
/// fsyncs to open the concurrency race window for investigation.
pub fn relaxed_durability() -> bool {
    std::env::var_os("ALT_RELAXED_DURABILITY").is_some()
}

fn pack_path(dir: &Path, seq: u32) -> PathBuf {
    dir.join(format!("pack-{seq:08}.altpack"))
}

fn idx_path(dir: &Path, seq: u32) -> PathBuf {
    dir.join(format!("pack-{seq:08}.altidx"))
}

/// Total on-disk bytes of the given pack files.
fn pack_bytes(dir: &Path, seqs: &[u32]) -> Result<u64, StoreError> {
    let mut total = 0u64;
    for &seq in seqs {
        total += std::fs::metadata(pack_path(dir, seq))?.len();
    }
    Ok(total)
}

/// An off-write-path fsync handle for the chunk store's active pack (see
/// [`ChunkStore::sink`]). Holds only the pack directory; it re-opens the
/// highest-seq (active) pack on each `fsync`, so a roll between calls is handled
/// by construction.
pub struct ChunkSink {
    dir: PathBuf,
}

impl ChunkSink {
    /// Fsyncs the active pack. A read+write handle (we only fsync, never write)
    /// is opened fresh so this needs no shared state with the live `ChunkStore`.
    pub fn fsync(&self) -> Result<(), StoreError> {
        if let Some(seq) = list_pack_seqs(&self.dir)?.into_iter().max() {
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(pack_path(&self.dir, seq))?;
            f.sync_all()?;
        }
        Ok(())
    }
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
        // create-new can lose a race to another process bringing the same
        // fresh store up; if it already exists, fall through and open it.
        match OpenOptions::new().create_new(true).write(true).open(path) {
            Ok(mut write) => {
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
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
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

/// The in-memory pack state a [`ChunkStore`] holds: sealed packs (mmapped),
/// the active pack (open for append), and the id → location index.
type PackState = (HashMap<u32, Sealed>, Active, HashMap<ChunkId, Location>);

/// Loads the on-disk pack set into the in-memory state a [`ChunkStore`] needs:
/// every sealed pack mmapped + indexed, and the active pack opened for append.
/// Factored out of the constructor so `sync_from_disk` can reload in place
/// without going through `ChunkStore` (which has a `Drop`, so its fields can't
/// be moved out).
fn load(dir: &Path) -> Result<PackState, StoreError> {
    std::fs::create_dir_all(dir)?;
    let seqs = list_pack_seqs(dir)?;
    let mut sealed = HashMap::new();
    let mut index = HashMap::new();

    let (&active_seq, sealed_seqs) = match seqs.split_last() {
        Some((last, rest)) => (last, rest),
        None => {
            let mut active = open_active(&pack_path(dir, 1), true)?;
            active.seq = 1;
            pack::fsync_dir(dir)?;
            return Ok((sealed, active, index));
        }
    };

    for &seq in sealed_seqs {
        let file = File::open(pack_path(dir, seq))?;
        let map = unsafe { Mmap::map(&file)? };
        pack::check_file_header(&map)?;
        let entries = match idx::read(&idx_path(dir, seq)) {
            Ok(entries) => entries,
            Err(_) => {
                // idx is a cache: rebuild from the pack and repair it
                let (recs, valid_len) = pack::scan(&map)?;
                if valid_len != map.len() as u64 {
                    return Err(StoreError::Format("sealed pack truncated"));
                }
                let entries: Vec<_> = recs.iter().map(|(hdr, off)| (hdr.id, *off)).collect();
                idx::write(&idx_path(dir, seq), &entries)?;
                pack::fsync_dir(dir)?;
                entries
            }
        };
        for (id, offset) in entries {
            index.insert(id, Location { seq, offset });
        }
        sealed.insert(seq, Sealed { map });
    }

    let mut active = open_active(&pack_path(dir, active_seq), false)?;
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
    Ok((sealed, active, index))
}

impl ChunkStore {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        Self::open_with(dir, Options::default())
    }

    pub fn open_with(dir: impl Into<PathBuf>, opts: Options) -> Result<Self, StoreError> {
        let dir = dir.into();
        let (sealed, active, index) = load(&dir)?;
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

    /// Reconciles in-memory state with the packs on disk (another process may
    /// have appended to the active pack, sealed/rolled it, or compacted). A
    /// no-op when nothing changed. Callers hold the odb write lock across this
    /// and their append, so the reconciled state stays current through the
    /// write. Counters and the delta cache (keyed by stable ids) survive a
    /// reload.
    pub fn sync_from_disk(&mut self) -> Result<(), StoreError> {
        if !self.disk_changed()? {
            return Ok(());
        }
        let (sealed, active, index) = load(&self.dir)?;
        self.sealed = sealed;
        self.active = active;
        self.index = index;
        Ok(())
    }

    /// Whether the on-disk pack set or the active pack's length differs from
    /// what we hold — the cheap check that gates a reload.
    fn disk_changed(&self) -> Result<bool, StoreError> {
        let seqs = list_pack_seqs(&self.dir)?;
        let mut known: Vec<u32> = self.sealed.keys().copied().collect();
        known.push(self.active.seq);
        known.sort_unstable();
        if seqs != known {
            return Ok(true);
        }
        let size = std::fs::metadata(pack_path(&self.dir, self.active.seq))?.len();
        Ok(size != self.active.len)
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

    /// Rewrites every live record into one fresh pack, dropping the dead
    /// weight `reencode_as_delta` leaves behind (superseded full records) and
    /// merging all packs into one. Records are copied verbatim, so ids,
    /// encodings and delta chains are preserved (a delta names its base by
    /// id, not offset). Identity-preserving — content, ids and the blobmap
    /// are untouched — so there is nothing to record in the op log.
    ///
    /// Crash-safe by construction: the compacted pack lands by atomic rename
    /// and the old packs survive until it is durable. Any crash leaves either
    /// the old packs intact (compaction did not happen) or both sets present
    /// (the higher-seq compacted pack wins, old records become dead) — never
    /// a missing or unopenable store.
    pub fn compact(&mut self) -> Result<CompactReport, StoreError> {
        self.active.write.sync_all()?; // every active record on disk first

        let old_seqs = list_pack_seqs(&self.dir)?;
        let bytes_before = pack_bytes(&self.dir, &old_seqs)?;
        let comp_seq = old_seqs.iter().copied().max().unwrap_or(0) + 1;

        // live records in ascending (seq, offset) — the bases-before-deltas
        // order the writer kept, so the compacted pack reads cache-friendly
        let mut live: Vec<(ChunkId, Location)> =
            self.index.iter().map(|(&id, &loc)| (id, loc)).collect();
        live.sort_unstable_by_key(|(_, loc)| (loc.seq, loc.offset));

        // write the compacted pack to a temp file, then rename it into place
        let final_path = pack_path(&self.dir, comp_seq);
        let tmp_path = final_path.with_extension("altpack.tmp");
        let mut entries: Vec<(ChunkId, u64)> = Vec::with_capacity(live.len());
        {
            let mut w = File::create(&tmp_path)?;
            w.write_all(&pack::file_header())?;
            let mut offset = pack::HEADER_LEN as u64;
            for (id, loc) in &live {
                let (header, payload) = self.read_record(*id, *loc)?;
                w.write_all(&header.encode())?;
                w.write_all(&payload)?;
                entries.push((*id, offset));
                offset += (REC_HEADER_LEN + payload.len()) as u64;
            }
            w.sync_all()?;
        }
        idx::write(&idx_path(&self.dir, comp_seq), &entries)?;
        std::fs::rename(&tmp_path, &final_path)?;
        pack::fsync_dir(&self.dir)?;

        // a fresh empty active above the compacted pack
        let active_seq = comp_seq + 1;
        let mut active = open_active(&pack_path(&self.dir, active_seq), true)?;
        active.seq = active_seq;
        pack::fsync_dir(&self.dir)?;

        // compacted pack + active are durable: now drop the old packs
        for &seq in &old_seqs {
            let _ = std::fs::remove_file(pack_path(&self.dir, seq));
            let _ = std::fs::remove_file(idx_path(&self.dir, seq));
        }
        pack::fsync_dir(&self.dir)?;

        // rebuild in-memory state. Ids are stable, so the delta cache (keyed
        // by id) stays valid across the relocation.
        let file = File::open(&final_path)?;
        let map = unsafe { Mmap::map(&file)? };
        self.sealed = HashMap::from([(comp_seq, Sealed { map })]);
        self.index = entries
            .iter()
            .map(|(id, off)| {
                (
                    *id,
                    Location {
                        seq: comp_seq,
                        offset: *off,
                    },
                )
            })
            .collect();
        self.active = active;
        self.counters.bytes_superseded = 0;

        let after_seqs = list_pack_seqs(&self.dir)?;
        Ok(CompactReport {
            packs_before: old_seqs.len(),
            packs_after: after_seqs.len(),
            bytes_before,
            bytes_after: pack_bytes(&self.dir, &after_seqs)?,
            records: entries.len(),
        })
    }

    /// Materializes every chunk exactly once, in delta-chain order, without
    /// re-hashing — the bulk read path for export / full-clone serving. Each
    /// full record is decoded once and its dependent deltas are resolved
    /// against the in-memory base, so a base shared by many deltas is never
    /// decoded twice (unlike per-object [`get`] under a bounded cache, where
    /// reading every object cold re-decodes shared bases repeatedly).
    ///
    /// No per-chunk hash: integrity is the caller's concern at the output
    /// boundary (export → `git fsck` + the round-trip fidelity matrix), a
    /// different trust context than a daily read. Use [`verify_chunk`] /
    /// `alt fsck` to scrub.
    pub fn for_each_decoded(&self, mut f: impl FnMut(ChunkId, &[u8])) -> Result<(), StoreError> {
        // chain forest: full records are roots, each delta hangs off the
        // base it names
        let mut deps: HashMap<ChunkId, Vec<ChunkId>> = HashMap::with_capacity(self.index.len());
        let mut roots: Vec<ChunkId> = Vec::new();
        for (&id, &loc) in &self.index {
            let header = self.read_header(id, loc)?;
            match header.encoding {
                ENC_DELTA => deps
                    .entry(self.read_base_id(id, loc)?)
                    .or_default()
                    .push(id),
                ENC_RAW | ENC_ZSTD => roots.push(id),
                _ => return Err(StoreError::Format("reserved record encoding")),
            }
        }

        // DFS from each root; an Rc carries a decoded base to all its
        // dependents without cloning, freed once the last one is resolved
        let mut emitted = 0usize;
        let mut stack: Vec<(ChunkId, Option<Rc<Vec<u8>>>)> =
            roots.into_iter().map(|id| (id, None)).collect();
        while let Some((id, base)) = stack.pop() {
            let bytes = Rc::new(self.decode_no_verify(id, base.as_ref().map(|b| &b[..]))?);
            f(id, &bytes);
            emitted += 1;
            for child in deps.remove(&id).unwrap_or_default() {
                stack.push((child, Some(Rc::clone(&bytes))));
            }
        }
        if emitted != self.index.len() {
            // a delta whose base is absent — only on-disk corruption
            return Err(StoreError::Format("bulk materialize missed chunks"));
        }
        Ok(())
    }

    /// Decodes a chunk's bytes without hashing, reading the stored payload
    /// zero-copy from the sealed mmap (the bulk path's hot read). `base` is
    /// `Some` for a delta record, `None` for a full one.
    fn decode_no_verify(&self, id: ChunkId, base: Option<&[u8]>) -> Result<Vec<u8>, StoreError> {
        let loc = *self.index.get(&id).ok_or(StoreError::NotFound(id))?;
        let header = self.read_header(id, loc)?;
        // payload: borrowed from the sealed mmap, owned only for the active
        // pack (a handful of records after a compaction)
        let owned;
        let payload: &[u8] = if loc.seq == self.active.seq {
            let mut buf = vec![0u8; header.stored_len as usize];
            pack::read_exact_at(
                &self.active.read,
                &mut buf,
                loc.offset + REC_HEADER_LEN as u64,
            )?;
            owned = buf;
            &owned
        } else {
            let sealed = &self.sealed[&loc.seq];
            let at = loc.offset as usize + REC_HEADER_LEN;
            sealed
                .map
                .get(at..at + header.stored_len as usize)
                .ok_or(StoreError::Corrupt {
                    id,
                    reason: "record out of bounds",
                })?
        };
        let data = match (header.encoding, base) {
            (ENC_RAW, None) => payload.to_vec(),
            (ENC_ZSTD, None) => zstd::decode_all(payload).map_err(|_| StoreError::Corrupt {
                id,
                reason: "zstd decode failed",
            })?,
            (ENC_DELTA, Some(base)) => {
                let z = payload.get(32..).ok_or(StoreError::Corrupt {
                    id,
                    reason: "delta payload too short",
                })?;
                delta::decompress_with_base(z, base, header.orig_len as usize).ok_or(
                    StoreError::Corrupt {
                        id,
                        reason: "delta decode failed",
                    },
                )?
            }
            _ => return Err(StoreError::Format("delta/full encoding mismatch")),
        };
        Self::sized(id, data, header.orig_len)
    }

    /// Decodes a single chunk without hashing, zero-copy from the sealed
    /// mmap for the common full-record case (the bulk blob-assembly read).
    /// Multi-chunk blob data and manifest nodes are full records, so they
    /// take the fast path; a delta falls back to the general chain resolve.
    pub(crate) fn decode_chunk_unverified(&self, id: ChunkId) -> Result<Vec<u8>, StoreError> {
        let loc = *self.index.get(&id).ok_or(StoreError::NotFound(id))?;
        if self.read_header(id, loc)?.encoding == ENC_DELTA {
            self.resolve(id, Verify::None)
        } else {
            self.decode_no_verify(id, None)
        }
    }

    /// Reads a chunk back, resolving any lineage delta chain. The default
    /// re-hashes once, at the requested address: a corrupt layer anywhere in
    /// the chain changes the assembled bytes, so a single boundary hash still
    /// detects it (tiered verification, M3.5 §阶段 B — the per-chunk hash was
    /// ~89% of read time). Deep per-layer verification is [`verify_chunk`];
    /// the blob assembler reads with [`read_unverified`] and hashes at the
    /// blob boundary instead.
    pub fn get(&self, id: ChunkId) -> Result<Vec<u8>, StoreError> {
        self.resolve(id, Verify::Boundary)
    }

    /// Deep scrub: resolve `id` re-hashing every layer from disk (bypassing
    /// the read cache), so corruption is localized to the failing layer
    /// rather than only detected at the boundary. The fsck path.
    pub fn verify_chunk(&self, id: ChunkId) -> Result<(), StoreError> {
        self.resolve(id, Verify::Deep).map(drop)
    }

    /// Resolves a chunk with no hashing (structural checks only). Internal:
    /// the caller must verify at a higher boundary — the blob hash.
    pub(crate) fn read_unverified(&self, id: ChunkId) -> Result<Vec<u8>, StoreError> {
        self.resolve(id, Verify::None)
    }

    /// Resolves a chunk re-hashing every layer; the bytes are returned so the
    /// blob deep-verify can reassemble and check the blob boundary too.
    pub(crate) fn read_deep(&self, id: ChunkId) -> Result<Vec<u8>, StoreError> {
        self.resolve(id, Verify::Deep)
    }

    fn resolve(&self, id: ChunkId, verify: Verify) -> Result<Vec<u8>, StoreError> {
        // descend: collect delta frames until a full record or a cached
        // base; iterative so chain length never threatens the stack
        let mut frames: Vec<(ChunkId, u32, Vec<u8>)> = Vec::new();
        let mut cur = id;
        let data: Vec<u8>;
        loop {
            // a deep scrub re-reads every layer from disk; the cache may hold
            // bytes that were only boundary-verified, so deep bypasses it
            if verify != Verify::Deep
                && !frames.is_empty()
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
                    data = Self::sized(cur, payload, header.orig_len)?;
                    if verify.hash_at(frames.is_empty()) {
                        Self::check_hash(cur, &data)?;
                    }
                    break;
                }
                ENC_ZSTD => {
                    let raw = zstd::decode_all(&payload[..]).map_err(|_| StoreError::Corrupt {
                        id: cur,
                        reason: "zstd decode failed",
                    })?;
                    data = Self::sized(cur, raw, header.orig_len)?;
                    if verify.hash_at(frames.is_empty()) {
                        Self::check_hash(cur, &data)?;
                    }
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

        // unwind: apply frames top-down, keeping every base hot. The last
        // frame popped is the requested id — its hash is the boundary.
        let mut data = data;
        let mut base_id = cur;
        while let Some((fid, orig_len, z)) = frames.pop() {
            let out = delta::decompress_with_base(&z, &data, orig_len as usize).ok_or(
                StoreError::Corrupt {
                    id: fid,
                    reason: "delta decode failed",
                },
            )?;
            let out = Self::sized(fid, out, orig_len)?;
            if verify.hash_at(frames.is_empty()) {
                Self::check_hash(fid, &out)?;
            }
            self.cache.lock().unwrap().put(base_id, Arc::new(data));
            data = out;
            base_id = fid;
        }
        Ok(data)
    }

    /// Structural length check (always run — cheap, catches gross corruption).
    fn sized(id: ChunkId, data: Vec<u8>, orig_len: u32) -> Result<Vec<u8>, StoreError> {
        if data.len() != orig_len as usize {
            return Err(StoreError::Corrupt {
                id,
                reason: "length mismatch",
            });
        }
        Ok(data)
    }

    /// BLAKE3 re-hash against the address (the expensive, deferrable check).
    fn check_hash(id: ChunkId, data: &[u8]) -> Result<(), StoreError> {
        if ChunkId::of(data) != id {
            return Err(StoreError::Corrupt {
                id,
                reason: "hash mismatch",
            });
        }
        Ok(())
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
        if !relaxed_durability() {
            self.active.write.sync_all()?;
        }
        Ok(())
    }

    /// Raw fsync of the active pack (no relaxed-durability gate) — the group
    /// commit layer decides whether/when to call it.
    pub fn fsync(&self) -> Result<(), StoreError> {
        self.active.write.sync_all()?;
        Ok(())
    }

    /// An independent fsync handle (the daemon's group commit fsyncs off the
    /// write path, without `&mut self`, so appends overlap the fsync). It
    /// re-finds the active pack on each call because the pack rolls; sealed
    /// packs are fsynced at seal time, so only the active one needs flushing.
    pub fn sink(&self) -> ChunkSink {
        ChunkSink {
            dir: self.dir.clone(),
        }
    }

    /// Our in-memory write cursor (bytes we have appended to the active pack).
    pub fn appended_len(&self) -> u64 {
        self.active.len
    }

    /// The active pack's true on-disk size (includes other writers' appends).
    pub fn pack_file_len(&self) -> Result<u64, StoreError> {
        Ok(std::fs::metadata(pack_path(&self.dir, self.active.seq))?.len())
    }

    /// Snapshots the current write cursor (active pack seq + appended bytes)
    /// so a later [`Self::rewind`] can drop everything appended after it.
    /// Held across one writer batch under the odb write lock.
    pub fn checkpoint(&self) -> ChunkCheckpoint {
        ChunkCheckpoint {
            pack_seq: self.active.seq,
            pack_len: self.active.len,
        }
    }

    /// Drops every record appended to the active pack after `ckpt` was taken:
    /// the active pack is truncated to `ckpt.pack_len` (with the trailing fsync
    /// that makes the new length durable), then the in-memory state is reloaded
    /// from disk via [`load`] — that rebuilds the index correctly even when the
    /// popped region's ids also existed in a sealed pack (`put` overwrote the
    /// sealed pointer with the active one). The delta cache is cleared because
    /// it may hold arc-cloned bytes that no longer have an on-disk source.
    /// Refuses if the active pack rolled between checkpoint and rewind (sealed
    /// packs are immutable, so a rolled-out region can't be safely undone here).
    pub fn rewind(&mut self, ckpt: ChunkCheckpoint) -> Result<(), StoreError> {
        if ckpt.pack_seq != self.active.seq {
            return Err(StoreError::Format(
                "chunk store rolled between checkpoint and rewind",
            ));
        }
        if ckpt.pack_len > self.active.len {
            return Err(StoreError::Format(
                "chunk rewind target above active cursor",
            ));
        }
        if ckpt.pack_len < pack::HEADER_LEN as u64 {
            return Err(StoreError::Format("chunk rewind target below header"));
        }
        self.active.write.set_len(ckpt.pack_len)?;
        self.active.write.sync_all()?;
        let (sealed, active, index) = load(&self.dir)?;
        self.sealed = sealed;
        self.active = active;
        self.index = index;
        self.cache.lock().unwrap().clear();
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
