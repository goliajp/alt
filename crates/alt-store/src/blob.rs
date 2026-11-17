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

mod open;
mod settle;

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::blobmap::BlobMap;
use crate::node::{NodeEntry, encode_node, parse_node};
use crate::{
    BlobId, ChunkCheckpoint, ChunkId, ChunkStore, CompactReport, Counters, Options, StoreError,
};

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

/// A point-in-time write cursor of a [`BlobStore`] for [`BlobStore::rewind`]:
/// the underlying chunk store's [`ChunkCheckpoint`] plus the blobmap's
/// appended length. Held across one writer batch under the odb write lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreCheckpoint {
    chunks: ChunkCheckpoint,
    blobmap_len: u64,
}

impl StoreCheckpoint {
    /// The active pack's sequence number and length, and the blob map's
    /// length, at the checkpoint.
    pub fn lens(&self) -> (u32, u64, u64) {
        let (seq, pack) = self.chunks.pack();
        (seq, pack, self.blobmap_len)
    }
}

/// Content-addressed byte-stream storage: CDC chunking + manifest trees
/// over a [`ChunkStore`].
pub struct BlobStore {
    // field order is drop order: the pack must sync before the blobmap so a
    // crash never leaves a durable map record pointing at lost chunks
    chunks: ChunkStore,
    /// opened by the first blob that is not a single chunk, or the first
    /// write: most reads never need it
    map: OnceLock<BlobMap>,
    /// the map's index runs, when the opener was told them
    map_runs: Option<Vec<(u64, u64)>>,
    /// what a blob map that does not read to its end means
    map_tail: crate::Tail,
    dir: PathBuf,
    opts: BlobOptions,
    /// The lock file of the database this store sits under, if any. Opening
    /// the map repairs a torn tail, which must not overlap a writer's
    /// append: a late open takes this lock shared, as the open of the store
    /// itself ran under it.
    open_lock: Option<PathBuf>,
    /// Whether the caller holds that lock exclusively right now (from
    /// `sync_from_disk` to `end_batch`), so a late open takes nothing.
    exclusive: bool,
}

impl BlobStore {
    /// Stores a byte stream, deduplicating chunk-wise against everything
    /// already in the store.
    pub fn put(&mut self, data: &[u8]) -> Result<BlobId, StoreError> {
        let id = BlobId::of(data);
        if self.contains(id)? {
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

        self.map_mut()?.append(id, level[0].id, data.len() as u64)?;
        Ok(id)
    }

    /// Materializes a blob. The chunks are read without per-chunk hashing
    /// and the assembled result is re-hashed once against the blob id, using
    /// the parallel BLAKE3 path — any corrupt chunk or manifest node changes
    /// the bytes, so one boundary hash still detects it. Deep
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
        // a blob of one chunk is that chunk under the same hash, with no
        // manifest: try it first, so the common read never opens the map
        let chunk = ChunkId(id.0);
        let single = match mode {
            ChunkRead::Deep => self.chunks.verify_chunk(chunk).map(|()| Vec::new()),
            ChunkRead::Fast => self.chunks.get(chunk),
            ChunkRead::Bulk => self.chunks.decode_chunk_unverified(chunk),
        };
        match single {
            Err(StoreError::NotFound(_)) => {}
            other => return other,
        }
        let Some((root, total_len)) = self.map()?.get(id)? else {
            return Err(StoreError::BlobNotFound(id));
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

    /// Enumerate this blob's leaf chunk ids in stream order. For a
    /// single-chunk blob the result is `[ChunkId(blob.0)]`; for a
    /// multi-chunk blob it walks the manifest tree and yields every
    /// leaf entry's chunk id. Pairs with [`ChunkStore::stat`] to
    /// describe how the blob is physically stored — the data the
    /// storage-view endpoint needs to surface tier / encoding /
    /// physical bytes per chunk.
    pub fn leaf_chunks(&self, id: BlobId) -> Result<Vec<ChunkId>, StoreError> {
        // single-chunk blob: chunk id coincides with blob id
        if self.chunks.contains(ChunkId(id.0))? {
            return Ok(vec![ChunkId(id.0)]);
        }
        let Some((root, _total_len)) = self.map()?.get(id)? else {
            return Ok(vec![ChunkId(id.0)]);
        };
        let mut out = Vec::new();
        self.collect_leaves(root, None, &mut out)?;
        Ok(out)
    }

    fn collect_leaves(
        &self,
        node_id: ChunkId,
        expect_level: Option<u8>,
        out: &mut Vec<ChunkId>,
    ) -> Result<(), StoreError> {
        let bytes = self.chunks.read_unverified(node_id)?;
        let (level, entries) = parse_node(&bytes)?;
        if expect_level.is_some_and(|expect| level != expect) {
            return Err(StoreError::Format("manifest level mismatch"));
        }
        for entry in entries {
            if level == 0 {
                out.push(entry.id);
            } else {
                self.collect_leaves(entry.id, Some(level - 1), out)?;
            }
        }
        Ok(())
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

    pub fn contains(&self, id: BlobId) -> Result<bool, StoreError> {
        Ok(self.chunks.contains(ChunkId(id.0))? || self.map()?.contains(id)?)
    }

    /// Re-encodes `blob` as a lineage delta against `base` when both are
    /// single-chunk blobs (small files — exactly where CDC cannot share
    /// and lineage wins). Multi-chunk blobs already dedup chunk-wise, so
    /// they are left alone. Returns whether a re-encoding happened.
    pub fn lineage_delta(&mut self, blob: BlobId, base: BlobId) -> Result<bool, StoreError> {
        if blob == base {
            return Ok(false);
        }
        let (blob_chunk, base_chunk) = (ChunkId(blob.0), ChunkId(base.0));
        if !self.chunks.contains(blob_chunk)? || !self.chunks.contains(base_chunk)? {
            return Ok(false);
        }
        if self.map()?.contains(blob)? || self.map()?.contains(base)? {
            return Ok(false);
        }
        let (blob, base) = (blob_chunk, base_chunk);
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
    pub fn is_multi_chunk(&self, id: BlobId) -> Result<bool, StoreError> {
        self.map()?.contains(id)
    }

    pub fn chunk_store(&self) -> &ChunkStore {
        &self.chunks
    }

    /// Durability point: syncs chunks first, then the blob map, so a crash
    /// between the two only ever loses map records, never referenced chunks.
    pub fn flush(&mut self) -> Result<(), StoreError> {
        self.chunks.flush()?;
        match self.map.get_mut() {
            Some(map) => map.sync(),
            None => Ok(()),
        }
    }

    /// Reconciles in-memory state with what other writers appended, before a
    /// write batch. Called by the odb under its write lock.
    pub fn sync_from_disk(&mut self) -> Result<(), StoreError> {
        self.exclusive = true;
        self.chunks.sync_from_disk()?;
        match self.map.get_mut() {
            Some(map) => map.sync_from_disk(),
            // not opened yet: opening reads the file as it is then
            None => Ok(()),
        }
    }

    /// Whether [`Self::index_tail`] has anything to do. A blob map this
    /// handle has not opened is not looked at.
    pub fn index_pending(&self) -> bool {
        self.chunks.index_pending() || self.map.get().is_some_and(BlobMap::index_pending)
    }

    /// Folds the records appended since the last index runs into new runs,
    /// once there are enough of them. Called by the odb under its write lock
    /// at the end of a batch.
    pub fn index_tail(&mut self) -> Result<(), StoreError> {
        self.chunks.index_tail()?;
        self.map_mut()?.index_tail(u64::MAX)
    }

    /// Raw fsync of chunks then the blob map, in that order — so a crash never
    /// leaves a durable map record pointing at lost chunks. Group commit calls
    /// this once for a batch of coalesced commits.
    pub fn fsync(&self) -> Result<(), StoreError> {
        self.chunks.fsync()?;
        match self.map.get() {
            Some(map) => map.fsync(),
            None => Ok(()),
        }
    }

    /// Our appended cursors: (chunk pack, blob map) bytes we have written.
    pub fn appended_lens(&self) -> Result<(u64, u64), StoreError> {
        Ok((self.chunks.appended_len(), self.map()?.appended_len()))
    }

    /// The true on-disk sizes of (chunk pack, blob map).
    pub fn file_lens(&self) -> Result<(u64, u64), StoreError> {
        Ok((self.chunks.pack_file_len()?, self.map()?.file_len()?))
    }

    /// Snapshots the write cursor (chunks + blobmap) so [`Self::rewind`] can
    /// later drop everything appended after it. Taken under the odb write lock
    /// at the start of a batch.
    pub fn checkpoint(&self) -> Result<StoreCheckpoint, StoreError> {
        Ok(StoreCheckpoint {
            chunks: self.chunks.checkpoint(),
            blobmap_len: self.map()?.appended_len(),
        })
    }

    /// Whether [`Self::rewind`] to `ckpt` can go ahead, changing nothing.
    pub fn check_rewind(&self, ckpt: StoreCheckpoint) -> Result<(), StoreError> {
        self.chunks.check_rewind(ckpt.chunks)?;
        self.map()?.check_rewind(ckpt.blobmap_len)
    }

    /// Drops every chunk + blobmap record appended after `ckpt` was taken,
    /// each layer truncating + fsyncing internally. The blobmap goes first,
    /// the inverse of the flush order: a crash between the two leaves chunks
    /// no record names, never a record naming chunks that are gone.
    pub fn rewind(&mut self, ckpt: StoreCheckpoint) -> Result<(), StoreError> {
        self.map_mut()?.rewind(ckpt.blobmap_len)?;
        self.chunks.rewind(ckpt.chunks)?;
        Ok(())
    }

    /// An independent fsync handle (chunks + blobmap) for the daemon's
    /// off-write-path group commit.
    pub fn sink(&self) -> Result<BlobSink, StoreError> {
        Ok(BlobSink {
            chunks: self.chunks.sink(),
            blobmap: self.map()?.sync_handle()?,
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
        let dir = alt_os::temp::tempdir().unwrap();
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
            assert!(store.contains(*id).unwrap());
            assert_eq!(&store.get(*id).unwrap(), case);
        }
    }

    #[test]
    fn manifest_tree_grows_levels_under_small_fanout() {
        let dir = alt_os::temp::tempdir().unwrap();
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
        let dir = alt_os::temp::tempdir().unwrap();
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
        let dir = alt_os::temp::tempdir().unwrap();
        let mut store = BlobStore::open(dir.path()).unwrap();
        let data = random_bytes(2 << 20, 5);
        let a = store.put(&data).unwrap();
        let written = store.counters().bytes_written;
        let b = store.put(&data).unwrap();
        assert_eq!(a, b);
        assert_eq!(store.counters().bytes_written, written);
    }

    #[test]
    fn checkpoint_then_rewind_undoes_appended_blobs() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut store = BlobStore::open(dir.path()).unwrap();
        let before = b"before checkpoint".to_vec();
        let multi = random_bytes(2 << 20, 11); // multi-chunk: writes blobmap too
        let id_before = store.put(&before).unwrap();
        let id_multi = store.put(&multi).unwrap();
        let ckpt = store.checkpoint().unwrap();
        let after = b"appended after checkpoint".to_vec();
        let after_multi = random_bytes(2 << 20, 12);
        let id_after = store.put(&after).unwrap();
        let id_after_multi = store.put(&after_multi).unwrap();
        assert!(store.contains(id_after).unwrap());
        assert!(store.contains(id_after_multi).unwrap());
        store.rewind(ckpt).unwrap();
        assert!(
            store.contains(id_before).unwrap(),
            "pre-checkpoint blob survives"
        );
        assert!(
            store.contains(id_multi).unwrap(),
            "pre-checkpoint multi survives"
        );
        assert_eq!(&store.get(id_before).unwrap(), &before);
        assert_eq!(&store.get(id_multi).unwrap(), &multi);
        assert!(
            !store.contains(id_after).unwrap(),
            "post-checkpoint blob gone"
        );
        assert!(
            !store.contains(id_after_multi).unwrap(),
            "post-checkpoint multi gone"
        );
        // re-open from disk: state matches the rewound view
        drop(store);
        let store = BlobStore::open(dir.path()).unwrap();
        assert!(store.contains(id_before).unwrap());
        assert!(store.contains(id_multi).unwrap());
        assert!(!store.contains(id_after).unwrap());
        assert!(!store.contains(id_after_multi).unwrap());
    }

    #[test]
    fn rewind_to_empty_checkpoint_yields_empty_store() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut store = BlobStore::open(dir.path()).unwrap();
        let ckpt = store.checkpoint().unwrap();
        let data = random_bytes(1 << 20, 13);
        let id = store.put(&data).unwrap();
        assert!(store.contains(id).unwrap());
        store.rewind(ckpt).unwrap();
        assert!(!store.contains(id).unwrap());
        drop(store);
        let store = BlobStore::open(dir.path()).unwrap();
        assert!(!store.contains(id).unwrap());
    }

    #[test]
    fn missing_blob_is_not_found() {
        let dir = alt_os::temp::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        let err = store.get(BlobId([7u8; 32])).unwrap_err();
        assert!(matches!(err, StoreError::BlobNotFound(_)));
    }
}
