//! The opened object database: the append files, the write lock over them
//! and the prism pipeline. [`crate::NativeOdb`] opens one on first use.

mod durable;
mod open;
mod sink;

pub(crate) use open::Mode;

use std::fs::File;
use std::path::PathBuf;
use std::sync::OnceLock;

use alt_git_codec::{ObjectId, ObjectKind, RawObject};
use alt_prism::Registry;
use alt_store::{BlobStore, CompactReport, StoreError, Tail};

use crate::head::Head;
use crate::lock::{hold_exclusive, release_exclusive};
use crate::map::{MapEntry, ObjectMap};
use crate::synced::Deferred;
use crate::tier1::{self, Tier1Map};
use crate::{BlobId, OdbError};

pub use sink::OdbSink;

/// The native object database: blob store + git-identity map.
///
/// Writes (`put`/`lineage_delta`/`compact`) are serialized across processes by
/// an exclusive `flock` on `odb.lock`, held from the first write of a batch
/// until `flush`. On acquiring it, the in-memory state is reconciled with
/// whatever other writers appended (`sync_from_disk`), so this batch dedups and
/// appends against the true current store. Reads take no lock.
pub(crate) struct Odb {
    pub(crate) blobs: BlobStore,
    pub(crate) map: ObjectMap,
    /// Tier 1 (prismatic) bookkeeping; empty when no prism ever fired.
    /// Opened by the first blob that is not stored verbatim, or the first
    /// write: most reads never need it.
    pub(crate) tier1: OnceLock<Tier1Map>,
    pub(crate) alt_dir: PathBuf,
    /// Prism pipeline: a `put` whose data round-trips through one of these
    /// is recorded in `tier1` and stored as deduplicated parts; everything
    /// else falls through to Tier 0 ([`alt_prism::Registry::decompose_verified`]
    /// enforces the byte-exact iron law). Default open() leaves it empty so
    /// behaviour matches the pre-prism store; consumers (alt-import, alt-cli)
    /// register the production set explicitly.
    pub(crate) registry: Registry,
    /// The advisory write lock; `held` tracks whether this batch owns it.
    pub(crate) lock: File,
    pub(crate) held: bool,
    /// What the lock file says about the store's files (see [`Head`]), as
    /// last read or written by this handle.
    pub(crate) head: Head,
    /// Group commit: a separate lock around the fsync, and a marker recording
    /// how far each append file is durable. A batch fsyncs only if no other
    /// writer already made its appends durable — so N concurrent commits
    /// coalesce to ~1 fsync (durability stops being per-commit, matching the
    /// no-fsync throughput while staying durable).
    pub(crate) sync_lock_path: PathBuf,
    pub(crate) durable_path: PathBuf,
    /// Deferred durability (the daemon): when set, `flush` skips its inline
    /// fsync (the daemon's group-commit coordinator fsyncs off the write path,
    /// via [`crate::NativeOdb::sink`], so concurrent appends overlap the fsync). flock
    /// does not serialize threads of one process, so the cross-process marker
    /// machinery alone cannot batch them — the daemon coordinates in-process.
    pub(crate) defer: bool,
    /// Counts deferred writes, so the daemon can tell whether a request wrote
    /// (and thus needs to wait for the group-commit fsync) without inspecting
    /// the command.
    pub(crate) write_count: u64,
    /// What a deferred flush last finished writing, for the sink to record.
    pub(crate) deferred: Deferred,
    /// What a Tier 1 map that does not read to its end means, for the open
    /// on first use.
    pub(crate) tier1_tail: Tail,
}

impl Odb {
    /// Append an extra prism on top of the default-registered set. Useful
    /// in tests; production code relies on [`default_registry`] capturing
    /// the canonical set so every reader can recompose what any writer
    /// stored.
    pub fn register_prism(&mut self, prism: Box<dyn alt_prism::Prism + Send + Sync>) {
        self.registry.register(prism);
    }

    /// Acquires the write lock for this batch (if not already held) and brings
    /// the in-memory state up to date with concurrent writers.
    fn acquire(&mut self) -> Result<(), OdbError> {
        if self.held {
            return Ok(());
        }
        hold_exclusive(&self.lock)?;
        self.held = true;
        self.blobs.sync_from_disk()?;
        if let Some(tier1) = self.tier1.get_mut() {
            tier1.sync_from_disk()?;
        }
        self.map.sync_from_disk()?;
        Ok(())
    }

    /// Releases the write lock (best-effort; the fd closing also releases it).
    fn release(&mut self) {
        if self.held {
            self.save_head();
            release_exclusive(&self.lock);
            self.held = false;
            self.blobs.end_batch();
        }
    }

    /// Read-path catch-up for a long-lived odb (the daemon between requests):
    /// brings the in-memory state up to date with on-disk writes by concurrent
    /// processes, then releases — reusing the proven write-path catch-up so a
    /// torn tail is handled correctly. Must not be called mid-batch (it is a
    /// request-boundary operation; a batch always reaches `flush` first).
    pub fn refresh(&mut self) -> Result<(), OdbError> {
        // a handle that is refreshed is one that lives across requests
        self.map.keep_reads();
        self.blobs.keep_reads();
        self.acquire()?;
        self.release();
        Ok(())
    }

    /// Stores one git object's canonical payload and records its identity
    /// bridge. The payload is re-hashed against `oid` — a wrong claimed id
    /// is rejected here rather than discovered at export. When the prism
    /// registry accepts the payload (byte-exact round trip), the parts are
    /// stored decomposed (Tier 1); otherwise verbatim (Tier 0). Either way
    /// the returned blob id is `BLAKE3(data)`. Idempotent.
    pub fn put(
        &mut self,
        oid: ObjectId,
        kind: ObjectKind,
        data: &[u8],
    ) -> Result<BlobId, OdbError> {
        self.acquire()?;
        // dedup against the now-current map (post-catch-up): a concurrent
        // writer may already have stored this object
        if let Some(entry) = self.map.by_git(&oid)? {
            return Ok(entry.alt);
        }
        let actual = ObjectId::hash_object(oid.algo(), kind, data);
        if actual != oid {
            return Err(OdbError::HashMismatch {
                claimed: oid,
                actual,
            });
        }
        let alt = self.put_blob(data)?;
        self.map.append(MapEntry {
            git: oid,
            alt,
            kind,
            size: data.len() as u64,
        })?;
        Ok(alt)
    }

    /// Stores content that has no git identity of its own (the contents
    /// behind git-lfs pointers): through the same prism and chunk pipeline
    /// as any blob, but absent from the object map, so it is never exported
    /// or pushed as a git object. Idempotent.
    pub fn put_content(&mut self, data: &[u8]) -> Result<BlobId, OdbError> {
        self.acquire()?;
        self.put_blob(data)
    }

    /// Reads back what [`Self::put_content`] stored, `None` when absent.
    pub fn get_content(&self, id: BlobId) -> Result<Option<Vec<u8>>, OdbError> {
        if !self.blobs.contains(id)? && !self.tier1()?.contains(id)? {
            return Ok(None);
        }
        self.fetch_blob(id).map(Some)
    }

    /// Routes a blob through the prism pipeline before falling back to
    /// verbatim chunk-store insertion. Shared by `put` (after the git-oid
    /// re-hash) and by any future writer that already trusts the blob id.
    fn put_blob(&mut self, data: &[u8]) -> Result<BlobId, OdbError> {
        let id = BlobId::of(data);
        if self.blobs.contains(id)? || self.tier1()?.contains(id)? {
            return Ok(id); // already stored, either tier
        }
        if let Some(t1) = self.registry.decompose_verified(data) {
            let mut part_ids = Vec::with_capacity(t1.decomposition.parts.len());
            for part in &t1.decomposition.parts {
                part_ids.push(self.blobs.put(part)?);
            }
            let record = tier1::encode_record(t1.prism, &t1.decomposition.recipe, &part_ids);
            let record_id = self.blobs.put(&record)?;
            self.tier1_mut()?.append(id, record_id)?;
            Ok(id)
        } else {
            Ok(self.blobs.put(data)?)
        }
    }

    /// Reads an object back by git id, materializing its canonical payload.
    /// A Tier 1 blob is recomposed from its parts and the recomposed bytes
    /// are re-hashed against the recorded blob id — the integrity boundary
    /// so a corrupt part or recipe surfaces, never wrong bytes.
    pub fn get(&self, oid: &ObjectId) -> Result<Option<RawObject>, OdbError> {
        let Some(entry) = self.map.by_git(oid)? else {
            return Ok(None);
        };
        let data = self.fetch_blob(entry.alt)?;
        if data.len() as u64 != entry.size {
            return Err(OdbError::SizeMismatch(*oid, entry.size));
        }
        Ok(Some(RawObject {
            kind: entry.kind,
            data,
        }))
    }

    /// Materialises a stored blob, recomposing through the prism registry
    /// when it lives in Tier 1. The recomposed bytes' BLAKE3 is checked
    /// against `alt` so a corrupted recipe or part surfaces here.
    pub(crate) fn fetch_blob(&self, alt: BlobId) -> Result<Vec<u8>, OdbError> {
        // stored verbatim is the common case; only a blob that is not can
        // be a decomposed one
        match self.blobs.get(alt) {
            Err(StoreError::BlobNotFound(_)) => {}
            other => return Ok(other?),
        }
        let Some(record_id) = self.tier1()?.get(alt)? else {
            return Err(StoreError::BlobNotFound(alt).into());
        };
        let record = self.blobs.get(record_id)?;
        let (prism, recipe, part_ids) =
            tier1::decode_record(&record).map_err(|e| OdbError::Tier1Record(alt, e))?;
        let parts: Vec<Vec<u8>> = part_ids
            .iter()
            .map(|p| self.blobs.get_unverified(*p))
            .collect::<Result<_, _>>()?;
        let part_refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
        let data = self
            .registry
            .recompose(prism, &recipe, &part_refs)
            .ok_or(OdbError::Tier1Recompose(alt))?;
        if BlobId::of(&data) != alt {
            return Err(OdbError::Tier1HashMismatch(alt));
        }
        Ok(data)
    }

    /// Identity/kind/size lookup without materializing the payload
    /// (`cat-file -t` / `-s` class queries).
    pub fn lookup(&self, oid: &ObjectId) -> Result<Option<MapEntry>, OdbError> {
        self.map.by_git(oid)
    }

    /// The git identities sharing one stored content (usually one; can be
    /// several across kinds or hash algorithms).
    pub fn lookup_by_alt(&self, id: BlobId) -> Result<Vec<MapEntry>, OdbError> {
        self.map.by_alt(id)
    }

    /// Resolve a short hex prefix (≥ 4 hex chars) to the unique git oid
    /// it matches. Returns up to `cap` matches so callers can detect
    /// ambiguity without paying for a full scan; cap-saturated results
    /// are treated as "ambiguous" by callers.
    ///
    /// Empty / too-short prefixes return an empty list. Prefix must be
    /// lowercase hex (the canonical form alt prints); the comparison is
    /// against `entry.git.to_string()` so non-hex chars yield no match.
    pub fn lookup_by_prefix(&self, prefix: &str, cap: usize) -> Result<Vec<MapEntry>, OdbError> {
        if prefix.len() < 4 || prefix.len() >= 64 {
            return Ok(Vec::new());
        }
        if !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(Vec::new());
        }
        self.map.by_prefix(&prefix.to_ascii_lowercase(), cap)
    }

    pub fn contains(&self, oid: &ObjectId) -> Result<bool, OdbError> {
        Ok(self.map.by_git(oid)?.is_some())
    }

    /// All mapped objects in import order (full-iteration basis for export
    /// and verification sweeps).
    pub fn entries(&self) -> Result<Vec<MapEntry>, OdbError> {
        self.map.entries()
    }

    /// Bulk-materializes every mapped object's bytes exactly once, without
    /// per-object re-hashing — the read path for export / full-clone serving.
    /// Single-chunk objects (the lineage-delta'd ones) go through the chunk
    /// store's decode-once forest, so a base shared by many versions is
    /// decoded just once; multi-chunk blobs (not delta'd) are assembled
    /// normally; Tier 1 (prism-decomposed) blobs come back through
    /// `fetch_blob`, which already recomposes through the registry and
    /// re-hashes the result against the recorded blob id (the prism
    /// integrity boundary — without it a corrupt part could yield wrong
    /// bytes silently). Tier 0 integrity still belongs to the output
    /// boundary (export → `git fsck`), not to each read.
    pub fn for_each_object_unverified(
        &self,
        mut f: impl FnMut(&MapEntry, &[u8]),
    ) -> Result<(), OdbError> {
        let entries = self.map.entries()?;
        let mut by_alt: std::collections::HashMap<BlobId, Vec<u32>> =
            std::collections::HashMap::with_capacity(entries.len());
        for (at, entry) in entries.iter().enumerate() {
            by_alt.entry(entry.alt).or_default().push(at as u32);
        }
        self.blobs
            .chunk_store()
            .for_each_decoded(|chunk_id, bytes| {
                // Skip chunks that aren't a top-level git object: this includes
                // prism parts and recipe-record blobs, which are stored
                // alongside but have no MapEntry of their own.
                for &at in by_alt.get(&BlobId(chunk_id.0)).into_iter().flatten() {
                    f(&entries[at as usize], bytes);
                }
            })?;
        for entry in &entries {
            if self.tier1()?.contains(entry.alt)? {
                // Tier 1: recompose through the registry; fetch_blob does the
                // recompose+rehash so we never emit silently-wrong bytes.
                let bytes = self.fetch_blob(entry.alt)?;
                f(entry, &bytes);
            } else if self.blobs.is_multi_chunk(entry.alt)? {
                let bytes = self.blobs.get_unverified(entry.alt)?;
                f(entry, &bytes);
            }
        }
        Ok(())
    }

    /// Re-encodes `old`'s content as a lineage delta against `new`'s
    /// (same-path predecessor → successor). Identity is untouched; only
    /// the storage form changes. Returns whether a re-encoding happened.
    pub fn lineage_delta(&mut self, old: &ObjectId, new: &ObjectId) -> Result<bool, OdbError> {
        self.acquire()?;
        let (Some(old_entry), Some(new_entry)) = (self.map.by_git(old)?, self.map.by_git(new)?)
        else {
            return Ok(false);
        };
        let (old_alt, new_alt) = (old_entry.alt, new_entry.alt);
        Ok(self.blobs.lineage_delta(old_alt, new_alt)?)
    }

    /// Compacts the underlying chunk store, reclaiming the dead weight left
    /// by lineage delta re-encoding. Object identities and the map are
    /// untouched — only physical storage is rewritten.
    pub fn compact(&mut self) -> Result<CompactReport, OdbError> {
        self.acquire()?;
        Ok(self.blobs.compact()?)
    }

    /// Number of mapped git objects.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn blobs(&self) -> &BlobStore {
        &self.blobs
    }
}

impl Drop for Odb {
    fn drop(&mut self) {
        // release the write lock if a batch never reached flush
        self.release();
    }
}
