//! The opened object database: the append files, the write lock over them
//! and the prism pipeline. [`crate::NativeOdb`] opens one on first use.

mod bound;
mod bulk;
mod confirmed;
mod cut;
mod durable;
mod follow;
mod lost;
mod open;
mod runs;
mod settle;
mod sink;

pub use cut::Discarded;
pub(crate) use cut::generation_kept;
pub use lost::Lost;
pub(crate) use open::Access;
pub(crate) use settle::lost;
pub use sink::OdbSink;

use std::fs::File;
use std::path::PathBuf;
use std::sync::OnceLock;

use alt_git_codec::{ObjectId, ObjectKind, RawObject};
use alt_prism::Registry;
use alt_store::{BlobStore, StoreError, Tail};

use crate::head::Head;
use crate::map::{MapEntry, ObjectMap};
use crate::synced::{Deferred, Lens};
use crate::tier1::{self, Tier1Map};
use crate::unsynced::Noted;
use crate::{BlobId, OdbError};

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
    /// How far this handle has seen the records of the blob map, the Tier 1
    /// map and `map.alt` name content that is stored, and the marker's
    /// generation then (see [`Self::settle`]).
    pub(crate) checked: Option<(u64, [u64; 3])>,
    /// What this handle cut off the ends of the files and nobody was told.
    pub(crate) discarded: Vec<Discarded>,
    /// Set for a handle that passes over records that were never synced
    /// and name content which is not stored: how far the lock file noted
    /// each file as synced when the handle was opened.
    pub(crate) confirmed: Option<Lens>,
    /// The lock file's note of unsynced appends as this handle last read
    /// it ([`crate::unsynced`]), shared with what judges a record of the
    /// files that does not read.
    pub(crate) left: Noted,
    /// The marker's generation as of what this handle holds of the
    /// store, and as the lock file notes it ([`crate::repair`]); how the
    /// store is opened again in the handle's place.
    pub(crate) generation: Option<u64>,
    pub(crate) noted: Option<u64>,
    pub(crate) opened: (alt_store::BlobOptions, bool),
}

impl Odb {
    /// Append an extra prism on top of the default-registered set. Useful
    /// in tests; production code relies on [`default_registry`] capturing
    /// the canonical set so every reader can recompose what any writer
    /// stored.
    pub fn register_prism(&mut self, prism: Box<dyn alt_prism::Prism + Send + Sync>) {
        self.registry.register(prism);
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
