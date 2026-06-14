//! Byte-stream (blob) layer over the chunk store: a blob is CDC-chunked,
//! each chunk stored once, and the chunk sequence recorded in a Merkle
//! manifest tree whose nodes are themselves chunks.
//!
//! A blob that chunks to a single piece is stored as that one chunk and
//! needs no manifest — its blob id and chunk id coincide by construction.
//! Multi-chunk blobs get manifest nodes (`ALTM` magic, version, level u8,
//! count u32, then `[chunk id 32B][span u64]` entries); a node at level 0
//! points at data chunks, higher levels point at nodes one level down. The
//! blob id → root mapping lives in `manifests/blobmap`.
//!
//! Reads re-hash the assembled bytes against the blob id, on top of the
//! per-chunk re-hash the chunk store already does.

use std::path::PathBuf;

use crate::blobmap::BlobMap;
use crate::{BlobId, ChunkId, ChunkStore, CompactReport, Counters, Options, StoreError};

/// How the blob assembler reads each chunk.
#[derive(Clone, Copy)]
enum ChunkRead {
    /// boundary-verified per the daily read (one hash at the chunk address)
    Fast,
    /// deep per-layer scrub (fsck)
    Deep,
    /// zero-copy, no hashing — the bulk export / clone-serve read
    Bulk,
}

const NODE_MAGIC: [u8; 4] = *b"ALTM";
const NODE_VERSION: u8 = 1;
/// Node header: magic + version + level u8 + count u32.
const NODE_HEADER_LEN: usize = 10;
/// Entry: chunk id 32 + span u64.
const NODE_ENTRY_LEN: usize = 40;

#[derive(Debug, Clone, Copy)]
struct NodeEntry {
    id: ChunkId,
    /// Bytes covered: chunk length at level 0, subtree total above.
    span: u64,
}

fn encode_node(level: u8, entries: &[NodeEntry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(NODE_HEADER_LEN + NODE_ENTRY_LEN * entries.len());
    out.extend_from_slice(&NODE_MAGIC);
    out.push(NODE_VERSION);
    out.push(level);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        out.extend_from_slice(&entry.id.0);
        out.extend_from_slice(&entry.span.to_le_bytes());
    }
    out
}

fn parse_node(data: &[u8]) -> Result<(u8, Vec<NodeEntry>), StoreError> {
    if data.len() < NODE_HEADER_LEN || data[..4] != NODE_MAGIC {
        return Err(StoreError::Format("bad manifest node header"));
    }
    if data[4] != NODE_VERSION {
        return Err(StoreError::Format("unsupported manifest node version"));
    }
    let level = data[5];
    let count = u32::from_le_bytes(data[6..10].try_into().unwrap()) as usize;
    if count
        .checked_mul(NODE_ENTRY_LEN)
        .and_then(|n| n.checked_add(NODE_HEADER_LEN))
        != Some(data.len())
    {
        return Err(StoreError::Format("manifest node length mismatch"));
    }
    let mut entries = Vec::with_capacity(count);
    for raw in data[NODE_HEADER_LEN..].chunks_exact(NODE_ENTRY_LEN) {
        let mut id = [0u8; 32];
        id.copy_from_slice(&raw[..32]);
        entries.push(NodeEntry {
            id: ChunkId(id),
            span: u64::from_le_bytes(raw[32..].try_into().unwrap()),
        });
    }
    Ok((level, entries))
}

#[derive(Debug, Clone, Copy)]
pub struct BlobOptions {
    pub chunks: Options,
    pub cdc: alt_cdc::Params,
    /// Max entries per manifest node. A stored parameter, not a format
    /// constant: changing it reshapes future manifests but never moves blob
    /// addresses.
    pub fanout: usize,
}

impl Default for BlobOptions {
    fn default() -> Self {
        Self {
            chunks: Options::default(),
            cdc: alt_cdc::DEFAULT_PARAMS,
            fanout: 4096,
        }
    }
}

/// Content-addressed byte-stream storage: CDC chunking + manifest trees
/// over a [`ChunkStore`].
pub struct BlobStore {
    // field order is drop order: the pack must sync before the blobmap so a
    // crash never leaves a durable map record pointing at lost chunks
    chunks: ChunkStore,
    map: BlobMap,
    opts: BlobOptions,
}

impl BlobStore {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        Self::open_with(dir, BlobOptions::default())
    }

    pub fn open_with(dir: impl Into<PathBuf>, opts: BlobOptions) -> Result<Self, StoreError> {
        assert!(opts.fanout >= 2, "manifest fanout must be at least 2");
        let dir = dir.into();
        let chunks = ChunkStore::open_with(&dir, opts.chunks)?;
        let map = BlobMap::open(&dir.join("manifests"))?;
        Ok(Self { chunks, map, opts })
    }

    /// Stores a byte stream, deduplicating chunk-wise against everything
    /// already in the store.
    pub fn put(&mut self, data: &[u8]) -> Result<BlobId, StoreError> {
        let id = BlobId::of(data);
        if self.contains(id) {
            return Ok(id);
        }

        let pieces: Vec<&[u8]> = alt_cdc::chunks(data, self.opts.cdc).collect();
        if pieces.len() <= 1 {
            // empty or single-chunk: the chunk is the blob, no manifest
            self.chunks.put(data)?;
            return Ok(id);
        }

        let mut level: Vec<NodeEntry> = pieces
            .iter()
            .map(|piece| {
                Ok(NodeEntry {
                    id: self.chunks.put(piece)?,
                    span: piece.len() as u64,
                })
            })
            .collect::<Result<_, StoreError>>()?;

        let mut depth = 0u8;
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(self.opts.fanout));
            for group in level.chunks(self.opts.fanout) {
                let node = encode_node(depth, group);
                next.push(NodeEntry {
                    id: self.chunks.put(&node)?,
                    span: group.iter().map(|e| e.span).sum(),
                });
            }
            depth = depth
                .checked_add(1)
                .ok_or(StoreError::Format("manifest tree too deep"))?;
            level = next;
        }

        self.map.append(id, level[0].id, data.len() as u64)?;
        Ok(id)
    }

    /// Materializes a blob. The chunks are read without per-chunk hashing
    /// and the assembled result is re-hashed once against the blob id, using
    /// the parallel BLAKE3 path — any corrupt chunk or manifest node changes
    /// the bytes, so one boundary hash still detects it (M3.5 §阶段 B). Deep
    /// per-chunk verification is [`verify`].
    pub fn get(&self, id: BlobId) -> Result<Vec<u8>, StoreError> {
        self.materialize(id, ChunkRead::Fast, true)
    }

    /// Deep scrub: re-hash every chunk and layer of the blob (fsck), then the
    /// blob boundary — corruption is localized, not just detected.
    pub fn verify(&self, id: BlobId) -> Result<(), StoreError> {
        self.materialize(id, ChunkRead::Deep, true).map(drop)
    }

    /// Assembles a blob with no hashing at all, reading chunks zero-copy from
    /// the sealed mmap — the bulk read for export / full-clone serving, where
    /// integrity is the output boundary's concern (`git fsck`), not each read.
    pub fn get_unverified(&self, id: BlobId) -> Result<Vec<u8>, StoreError> {
        self.materialize(id, ChunkRead::Bulk, false)
    }

    fn materialize(
        &self,
        id: BlobId,
        mode: ChunkRead,
        hash_boundary: bool,
    ) -> Result<Vec<u8>, StoreError> {
        let Some((root, total_len)) = self.map.get(id) else {
            // no manifest: the blob is a single chunk under the same hash
            let chunk = ChunkId(id.0);
            let res = match mode {
                ChunkRead::Deep => self.chunks.verify_chunk(chunk).map(|()| Vec::new()),
                ChunkRead::Fast => self.chunks.get(chunk),
                ChunkRead::Bulk => self.chunks.decode_chunk_unverified(chunk),
            };
            return match res {
                Ok(data) => Ok(data),
                Err(StoreError::NotFound(_)) => Err(StoreError::BlobNotFound(id)),
                Err(e) => Err(e),
            };
        };
        let mut out = Vec::with_capacity(total_len as usize);
        self.walk(root, None, &mut out, mode)?;
        if out.len() as u64 != total_len {
            return Err(StoreError::Corrupt {
                id: root,
                reason: "manifest span mismatch",
            });
        }
        if hash_boundary && BlobId::of(&out) != id {
            return Err(StoreError::Corrupt {
                id: root,
                reason: "blob hash mismatch",
            });
        }
        Ok(out)
    }

    fn walk(
        &self,
        node_id: ChunkId,
        expect_level: Option<u8>,
        out: &mut Vec<u8>,
        mode: ChunkRead,
    ) -> Result<(), StoreError> {
        let read = |id| match mode {
            ChunkRead::Deep => self.chunks.read_deep(id),
            ChunkRead::Fast => self.chunks.read_unverified(id),
            ChunkRead::Bulk => self.chunks.decode_chunk_unverified(id),
        };
        let bytes = read(node_id)?;
        let (level, entries) = parse_node(&bytes)?;
        if expect_level.is_some_and(|expect| level != expect) {
            return Err(StoreError::Format("manifest level mismatch"));
        }
        for entry in entries {
            if level == 0 {
                let piece = read(entry.id)?;
                if piece.len() as u64 != entry.span {
                    return Err(StoreError::Corrupt {
                        id: entry.id,
                        reason: "manifest span mismatch",
                    });
                }
                out.extend_from_slice(&piece);
            } else {
                self.walk(entry.id, Some(level - 1), out, mode)?;
            }
        }
        Ok(())
    }

    pub fn contains(&self, id: BlobId) -> bool {
        self.map.contains(id) || self.chunks.contains(ChunkId(id.0))
    }

    /// Re-encodes `blob` as a lineage delta against `base` when both are
    /// single-chunk blobs (small files — exactly where CDC cannot share
    /// and lineage wins). Multi-chunk blobs already dedup chunk-wise, so
    /// they are left alone. Returns whether a re-encoding happened.
    pub fn lineage_delta(&mut self, blob: BlobId, base: BlobId) -> Result<bool, StoreError> {
        if blob == base || self.map.contains(blob) || self.map.contains(base) {
            return Ok(false);
        }
        let (blob, base) = (ChunkId(blob.0), ChunkId(base.0));
        if !self.chunks.contains(blob) || !self.chunks.contains(base) {
            return Ok(false);
        }
        self.chunks.reencode_as_delta(blob, base)
    }

    /// Chunk-level dedup/volume accounting for this session.
    pub fn counters(&self) -> Counters {
        self.chunks.counters()
    }

    /// Compacts the underlying chunk store, reclaiming the dead weight left
    /// by lineage delta re-encoding. Blob ids are content hashes and the
    /// blobmap is unaffected — only physical chunk storage is rewritten.
    pub fn compact(&mut self) -> Result<CompactReport, StoreError> {
        self.chunks.compact()
    }

    /// Whether `id` is a multi-chunk blob (has a manifest). Single-chunk
    /// blobs are stored directly as one chunk under the same address.
    pub fn is_multi_chunk(&self, id: BlobId) -> bool {
        self.map.contains(id)
    }

    pub fn chunk_store(&self) -> &ChunkStore {
        &self.chunks
    }

    /// Durability point: syncs chunks first, then the blob map, so a crash
    /// between the two only ever loses map records, never referenced chunks.
    pub fn flush(&mut self) -> Result<(), StoreError> {
        self.chunks.flush()?;
        self.map.sync()
    }

    /// Reconciles in-memory state with what other writers appended, before a
    /// write batch. Called by the odb under its write lock.
    pub fn sync_from_disk(&mut self) -> Result<(), StoreError> {
        self.chunks.sync_from_disk()?;
        self.map.sync_from_disk()
    }

    /// Raw fsync of chunks then the blob map, in that order — so a crash never
    /// leaves a durable map record pointing at lost chunks. Group commit calls
    /// this once for a batch of coalesced commits.
    pub fn fsync(&self) -> Result<(), StoreError> {
        self.chunks.fsync()?;
        self.map.fsync()
    }

    /// Our appended cursors: (chunk pack, blob map) bytes we have written.
    pub fn appended_lens(&self) -> (u64, u64) {
        (self.chunks.appended_len(), self.map.appended_len())
    }

    /// The true on-disk sizes of (chunk pack, blob map).
    pub fn file_lens(&self) -> Result<(u64, u64), StoreError> {
        Ok((self.chunks.pack_file_len()?, self.map.file_len()?))
    }

    /// An independent fsync handle (chunks + blobmap) for the daemon's
    /// off-write-path group commit.
    pub fn sink(&self) -> Result<BlobSink, StoreError> {
        Ok(BlobSink {
            chunks: self.chunks.sink(),
            blobmap: self.map.sync_handle()?,
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
        self.chunks.fsync()?;
        self.blobmap.sync_all()?;
        Ok(())
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
    fn round_trips_blobs_of_every_shape() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = BlobStore::open(dir.path()).unwrap();
        let cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"tiny".to_vec(),
            random_bytes(10_000, 1),  // single chunk (under min)
            random_bytes(2 << 20, 2), // multi-chunk, one manifest level
            vec![0u8; 1 << 20],       // compressible multi-chunk
        ];
        let ids: Vec<BlobId> = cases.iter().map(|c| store.put(c).unwrap()).collect();
        for (case, id) in cases.iter().zip(&ids) {
            assert_eq!(*id, BlobId::of(case));
            assert!(store.contains(*id));
            assert_eq!(&store.get(*id).unwrap(), case);
        }
    }

    #[test]
    fn manifest_tree_grows_levels_under_small_fanout() {
        let dir = tempfile::tempdir().unwrap();
        let opts = BlobOptions {
            fanout: 3,
            ..BlobOptions::default()
        };
        let mut store = BlobStore::open_with(dir.path(), opts).unwrap();
        // ~64 chunks at the default 64KiB average -> several levels at fanout 3
        let data = random_bytes(4 << 20, 3);
        let id = store.put(&data).unwrap();
        assert_eq!(store.get(id).unwrap(), data);
    }

    #[test]
    fn shifted_blob_shares_most_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = BlobStore::open(dir.path()).unwrap();
        let data = random_bytes(4 << 20, 4);
        store.put(&data).unwrap();
        let written_first = store.counters().bytes_written;

        let mut shifted = data.clone();
        shifted.insert(100, 0xAB);
        let id = store.put(&shifted).unwrap();
        assert_eq!(store.get(id).unwrap(), shifted);

        let written_second = store.counters().bytes_written - written_first;
        assert!(
            written_second * 4 < written_first,
            "a 1-byte insert must reuse most chunks: \
             first put wrote {written_first}, second {written_second}"
        );
        assert!(store.counters().dedup_hits > 0);
    }

    #[test]
    fn identical_blob_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = BlobStore::open(dir.path()).unwrap();
        let data = random_bytes(2 << 20, 5);
        let a = store.put(&data).unwrap();
        let written = store.counters().bytes_written;
        let b = store.put(&data).unwrap();
        assert_eq!(a, b);
        assert_eq!(store.counters().bytes_written, written);
    }

    #[test]
    fn missing_blob_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        let err = store.get(BlobId([7u8; 32])).unwrap_err();
        assert!(matches!(err, StoreError::BlobNotFound(_)));
    }
}
