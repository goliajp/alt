// CARVE-OUT: pre-existing oversized module; split pieces out as they are touched
//! alt native object database: the logical layer of the `.alt` store.
//!
//! Canonical git object bytes (the payload behind `"<kind> <size>\0"`) live
//! in the chunk/blob store under their BLAKE3 address; `map.alt` bridges
//! git identity (sha1/sha256) to that address plus the kind and size the
//! canonical header is rebuilt from. Export therefore reproduces git bytes
//! by construction — fidelity is structural, not tested-in.
//!
//! Layout under the `.alt` root:
//!
//! - `store/` — chunk packs + manifests + blobmap ([`alt_store`])
//! - `map.alt` — sha ↔ blake3 ↔ (kind, size), append-only, checksummed
//!
//! Writes re-hash the payload against the claimed git oid (a wrong binding
//! recorded at import would otherwise survive until export), and reads
//! inherit the blob layer's BLAKE3 verification.

mod check;
mod error;
mod lock;
mod map;
mod odb;
mod registry;
mod storage_view;
mod tier1;

use std::path::PathBuf;
use std::sync::OnceLock;

use alt_git_codec::{ObjectId, ObjectKind, RawObject};
use alt_store::{BlobOptions, BlobStore, CompactReport, StoreCheckpoint};

pub use alt_store::{BlobId, held};
pub use check::OdbChecked;
pub use error::OdbError;
pub use map::{MapEntry, ObjectMap};
use odb::Odb;
pub use odb::OdbSink;
pub use registry::default_registry;
pub use storage_view::{
    ChunkInfo, ChunkLayout, PrismStats, StorageStats, StorageView, Tier1Layout,
};

/// A point-in-time write cursor of a [`NativeOdb`] for [`NativeOdb::rewind`]:
/// snapshots each append file's current length (chunks pack, blobmap, tier1,
/// map.alt) so a failed receive-pack ingest can roll back every newly written
/// object without leaving orphans. Take it under the odb write lock and pass
/// the same value back to `rewind` under the same lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OdbCheckpoint {
    pub(crate) blobs: StoreCheckpoint,
    pub(crate) tier1_len: u64,
    pub(crate) map_len: u64,
}

/// The native object database: blob store + git-identity map.
///
/// Writes (`put`/`lineage_delta`/`compact`) are serialized across processes by
/// an exclusive `flock` on `odb.lock`, held from the first write of a batch
/// until `flush`. On acquiring it, the in-memory state is reconciled with
/// whatever other writers appended, so this batch dedups and appends against
/// the true current store. Reads take no lock.
///
/// The store's files are opened by the first call that needs them: a
/// database made with [`Self::deferred`] costs nothing until then, so a
/// command that never reads an object never pays for the object store.
pub struct NativeOdb {
    alt_dir: PathBuf,
    opts: BlobOptions,
    odb: OnceLock<Odb>,
    /// whether the store's less used files wait for their own first use
    late: bool,
    defer: bool,
}

impl NativeOdb {
    /// Opens (or creates) the database under the `.alt` root directory.
    pub fn open(alt_dir: impl Into<PathBuf>) -> Result<Self, OdbError> {
        Self::open_with(alt_dir, BlobOptions::default())
    }

    pub fn open_with(alt_dir: impl Into<PathBuf>, opts: BlobOptions) -> Result<Self, OdbError> {
        let this = Self {
            alt_dir: alt_dir.into(),
            opts,
            odb: OnceLock::new(),
            late: false,
            defer: false,
        };
        this.odb()?;
        Ok(this)
    }

    /// Names the database under the `.alt` root directory without opening
    /// it. Each of its files is opened by the first read or write that
    /// needs it, which is also where damage to that file is reported; a
    /// read of an ordinary object touches only the chunk store and
    /// `map.alt`.
    pub fn deferred(alt_dir: impl Into<PathBuf>) -> Self {
        Self {
            alt_dir: alt_dir.into(),
            opts: BlobOptions::default(),
            odb: OnceLock::new(),
            late: true,
            defer: false,
        }
    }

    fn start(&self) -> Result<Odb, OdbError> {
        let mut odb = Odb::open_with(&self.alt_dir, self.opts, self.late)?;
        odb.set_defer_durability(self.defer);
        Ok(odb)
    }

    fn odb(&self) -> Result<&Odb, OdbError> {
        if let Some(odb) = self.odb.get() {
            return Ok(odb);
        }
        let odb = self.start()?;
        Ok(self.odb.get_or_init(|| odb))
    }

    fn odb_mut(&mut self) -> Result<&mut Odb, OdbError> {
        if self.odb.get().is_none() {
            let odb = self.start()?;
            let _ = self.odb.set(odb);
        }
        Ok(self.odb.get_mut().expect("the store was opened just above"))
    }

    /// Append an extra prism on top of the default-registered set. Useful
    /// in tests; production code relies on [`default_registry`] capturing
    /// the canonical set so every reader can recompose what any writer
    /// stored.
    pub fn register_prism(
        &mut self,
        prism: Box<dyn alt_prism::Prism + Send + Sync>,
    ) -> Result<(), OdbError> {
        self.odb_mut()?.register_prism(prism);
        Ok(())
    }

    /// Read-path catch-up for a long-lived odb (the daemon between requests):
    /// brings the in-memory state up to date with on-disk writes by concurrent
    /// processes. Must not be called mid-batch (it is a request-boundary
    /// operation; a batch always reaches `flush` first).
    pub fn refresh(&mut self) -> Result<(), OdbError> {
        match self.odb.get_mut() {
            Some(odb) => odb.refresh(),
            // not opened yet: the first use reads the store as it is then
            None => Ok(()),
        }
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
        self.odb_mut()?.put(oid, kind, data)
    }

    /// Stores content that has no git identity of its own (the contents
    /// behind git-lfs pointers): through the same prism and chunk pipeline
    /// as any blob, but absent from the object map, so it is never exported
    /// or pushed as a git object. Idempotent.
    pub fn put_content(&mut self, data: &[u8]) -> Result<BlobId, OdbError> {
        self.odb_mut()?.put_content(data)
    }

    /// Reads back what [`Self::put_content`] stored, `None` when absent.
    pub fn get_content(&self, id: BlobId) -> Result<Option<Vec<u8>>, OdbError> {
        self.odb()?.get_content(id)
    }

    /// Reads an object back by git id, materializing its canonical payload.
    /// A Tier 1 blob is recomposed from its parts and the recomposed bytes
    /// are re-hashed against the recorded blob id — the integrity boundary
    /// so a corrupt part or recipe surfaces, never wrong bytes.
    pub fn get(&self, oid: &ObjectId) -> Result<Option<RawObject>, OdbError> {
        self.odb()?.get(oid)
    }

    /// Identity/kind/size lookup without materializing the payload
    /// (`cat-file -t` / `-s` class queries).
    pub fn lookup(&self, oid: &ObjectId) -> Result<Option<MapEntry>, OdbError> {
        self.odb()?.lookup(oid)
    }

    /// The git identities sharing one stored content (usually one; can be
    /// several across kinds or hash algorithms).
    pub fn lookup_by_alt(&self, id: BlobId) -> Result<Vec<MapEntry>, OdbError> {
        self.odb()?.lookup_by_alt(id)
    }

    /// Resolve a short hex prefix (≥ 4 hex chars) to the git oids it
    /// matches, at most `cap` of them so callers can detect ambiguity
    /// without paying for every match. Too-short or non-hex prefixes match
    /// nothing.
    pub fn lookup_by_prefix(&self, prefix: &str, cap: usize) -> Result<Vec<MapEntry>, OdbError> {
        self.odb()?.lookup_by_prefix(prefix, cap)
    }

    pub fn contains(&self, oid: &ObjectId) -> Result<bool, OdbError> {
        self.odb()?.contains(oid)
    }

    /// All mapped objects in import order (full-iteration basis for export
    /// and verification sweeps).
    pub fn entries(&self) -> Result<Vec<MapEntry>, OdbError> {
        self.odb()?.entries()
    }

    /// Bulk-materializes every mapped object's bytes exactly once, without
    /// per-object re-hashing — the read path for export / full-clone serving.
    /// Tier 1 (prism-decomposed) blobs are still recomposed and re-hashed
    /// against their blob id; Tier 0 integrity belongs to the output
    /// boundary (export → `git fsck`), not to each read.
    pub fn for_each_object_unverified(
        &self,
        f: impl FnMut(&MapEntry, &[u8]),
    ) -> Result<(), OdbError> {
        self.odb()?.for_each_object_unverified(f)
    }

    /// Re-encodes `old`'s content as a lineage delta against `new`'s
    /// (same-path predecessor → successor). Identity is untouched; only
    /// the storage form changes. Returns whether a re-encoding happened.
    pub fn lineage_delta(&mut self, old: &ObjectId, new: &ObjectId) -> Result<bool, OdbError> {
        self.odb_mut()?.lineage_delta(old, new)
    }

    /// Compacts the underlying chunk store, reclaiming the dead weight left
    /// by lineage delta re-encoding. Object identities and the map are
    /// untouched — only physical storage is rewritten.
    pub fn compact(&mut self) -> Result<CompactReport, OdbError> {
        self.odb_mut()?.compact()
    }

    /// Number of mapped git objects.
    pub fn len(&self) -> Result<usize, OdbError> {
        Ok(self.odb()?.len())
    }

    pub fn is_empty(&self) -> Result<bool, OdbError> {
        Ok(self.odb()?.is_empty())
    }

    pub fn blobs(&self) -> Result<&BlobStore, OdbError> {
        Ok(self.odb()?.blobs())
    }

    /// Finish an unfinished object batch, including after a command error.
    /// Unlike `flush`, an idle call neither synchronizes nor counts a write.
    /// Daemons call this before handing a request to a durability barrier so
    /// a failed put cannot retain its exclusive lock across requests.
    pub fn flush_pending(&mut self) -> Result<(), OdbError> {
        match self.odb.get_mut() {
            Some(odb) => odb.flush_pending(),
            None => Ok(()),
        }
    }

    /// Durability point: blob store first (chunks before blobmap inside),
    /// then `map.alt`, so a crash never leaves a durable identity record
    /// pointing at lost content.
    pub fn flush(&mut self) -> Result<(), OdbError> {
        self.odb_mut()?.flush()
    }

    /// Snapshots the write cursor across all append files, so a later
    /// [`Self::rewind`] can roll back every object written after it.
    /// Acquires the odb write lock, reconciles in-memory state with disk,
    /// captures the cursor, then releases the lock.
    pub fn checkpoint(&mut self) -> Result<OdbCheckpoint, OdbError> {
        self.odb_mut()?.checkpoint()
    }

    /// Cheap cursor snapshot of the in-memory write offsets, without touching
    /// the file lock or reconciling against the disk. Safe only when the
    /// caller already holds a write barrier and just wants a post-batch
    /// snapshot to compare against the pre-batch one.
    pub fn cursor(&self) -> Result<OdbCheckpoint, OdbError> {
        self.odb()?.cursor()
    }

    /// Rolls every odb write since `ckpt` back: truncates each append file to
    /// its captured length, drops the affected index runs and in-memory
    /// state, and fsyncs the new lengths. Held under the odb write lock.
    pub fn rewind(&mut self, ckpt: OdbCheckpoint) -> Result<(), OdbError> {
        self.odb_mut()?.rewind(ckpt)
    }

    /// Turns deferred durability on or off (the daemon turns it on so its
    /// group-commit coordinator batches fsyncs in-process; the direct CLI
    /// leaves it off and fsyncs inline).
    pub fn set_defer_durability(&mut self, on: bool) {
        self.defer = on;
        if let Some(odb) = self.odb.get_mut() {
            odb.set_defer_durability(on);
        }
    }

    /// A monotonic count of deferred writes — the daemon snapshots it around a
    /// request to tell whether the command wrote.
    pub fn write_count(&self) -> u64 {
        self.odb.get().map_or(0, Odb::write_count)
    }

    /// An independent fsync handle (chunks → blobmap → tier1 → `map.alt`)
    /// for the daemon's off-write-path group commit.
    pub fn sink(&self) -> Result<OdbSink, OdbError> {
        self.odb()?.sink()
    }

    /// How one object is physically stored (tier, prism parts, chunk
    /// encodings); `None` when the object is absent.
    pub fn storage_view(&self, oid: &ObjectId) -> Result<Option<StorageView>, OdbError> {
        self.odb()?.storage_view(oid)
    }

    /// Store-wide totals by tier, chunk encoding and prism.
    pub fn storage_stats(&self) -> Result<StorageStats, OdbError> {
        self.odb()?.storage_stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_git_codec::HashAlgo;
    use alt_prism::Prism;

    fn put_one(
        odb: &mut NativeOdb,
        algo: HashAlgo,
        kind: ObjectKind,
        data: &[u8],
    ) -> (ObjectId, BlobId) {
        let oid = ObjectId::hash_object(algo, kind, data);
        let alt = odb.put(oid, kind, data).unwrap();
        (oid, alt)
    }

    #[test]
    fn round_trips_all_kinds_under_both_algos() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let blob = b"file contents\n".to_vec();
        // a minimal valid-shape tree entry is not required for storage: the
        // odb stores canonical payloads opaquely
        let tree = b"100644 a\0"
            .iter()
            .chain([0u8; 20].iter())
            .copied()
            .collect::<Vec<u8>>();
        let cases: Vec<(ObjectKind, Vec<u8>)> = vec![
            (ObjectKind::Blob, blob),
            (ObjectKind::Tree, tree),
            (ObjectKind::Commit, b"tree 0000\n".to_vec()),
            (ObjectKind::Tag, b"object 0000\n".to_vec()),
        ];
        for algo in [HashAlgo::Sha1, HashAlgo::Sha256] {
            for (kind, data) in &cases {
                let (oid, alt) = put_one(&mut odb, algo, *kind, data);
                let back = odb.get(&oid).unwrap().unwrap();
                assert_eq!(back.kind, *kind);
                assert_eq!(&back.data, data);
                let entry = odb.lookup(&oid).unwrap().unwrap();
                assert_eq!(entry.size, data.len() as u64);
                assert_eq!(entry.alt, alt);
                assert!(odb.lookup_by_alt(alt).unwrap().iter().any(|e| e.git == oid));
            }
        }
        assert_eq!(odb.len().unwrap(), 8);
    }

    #[test]
    fn content_without_git_identity_stays_out_of_the_map() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let data = vec![5u8; 300_000];
        let id = odb.put_content(&data).unwrap();
        assert_eq!(id, BlobId::of(&data));
        odb.flush().unwrap();
        drop(odb);
        let odb = NativeOdb::open(dir.path()).unwrap();
        assert_eq!(odb.get_content(id).unwrap(), Some(data));
        assert!(odb.is_empty().unwrap());
        assert_eq!(odb.get_content(BlobId::of(b"absent")).unwrap(), None);
    }

    #[test]
    fn wrong_claimed_oid_is_rejected_at_write() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"other content");
        let err = odb
            .put(oid, ObjectKind::Blob, b"actual content")
            .unwrap_err();
        assert!(matches!(err, OdbError::HashMismatch { .. }));
        assert!(
            !odb.contains(&oid).unwrap(),
            "a rejected put must record nothing"
        );
    }

    #[test]
    fn empty_tree_is_a_first_class_object() {
        // native trees may be empty (empty-directory model lands here);
        // export degrades per git semantics, storage does not special-case
        let dir = alt_os::temp::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let (oid, _) = put_one(&mut odb, HashAlgo::Sha1, ObjectKind::Tree, b"");
        assert_eq!(
            format!("{oid}"),
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            "the canonical empty tree id is pinned by git history"
        );
        let back = odb.get(&oid).unwrap().unwrap();
        assert_eq!(back.kind, ObjectKind::Tree);
        assert!(back.data.is_empty());
    }

    #[test]
    fn identical_payload_under_two_kinds_keeps_both_identities() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let data = b"";
        let (blob_oid, blob_alt) = put_one(&mut odb, HashAlgo::Sha1, ObjectKind::Blob, data);
        let (tree_oid, tree_alt) = put_one(&mut odb, HashAlgo::Sha1, ObjectKind::Tree, data);
        assert_ne!(blob_oid, tree_oid, "git ids differ via the header");
        assert_eq!(blob_alt, tree_alt, "content is stored exactly once");
        assert_eq!(odb.get(&blob_oid).unwrap().unwrap().kind, ObjectKind::Blob);
        assert_eq!(odb.get(&tree_oid).unwrap().unwrap().kind, ObjectKind::Tree);
        assert_eq!(odb.lookup_by_alt(blob_alt).unwrap().len(), 2);
    }

    #[test]
    fn duplicate_put_is_idempotent() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let (oid, a) = put_one(&mut odb, HashAlgo::Sha1, ObjectKind::Blob, b"dup");
        let b = odb.put(oid, ObjectKind::Blob, b"dup").unwrap();
        assert_eq!(a, b);
        assert_eq!(odb.len().unwrap(), 1);
    }

    #[test]
    fn missing_object_reads_as_none() {
        let dir = alt_os::temp::tempdir().unwrap();
        let odb = NativeOdb::open(dir.path()).unwrap();
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"absent");
        assert!(odb.get(&oid).unwrap().is_none());
        assert!(odb.lookup(&oid).unwrap().is_none());
    }

    #[test]
    fn concurrent_writers_store_everything_without_corruption() {
        use std::sync::{Arc, Barrier};

        let dir = alt_os::temp::tempdir().unwrap();
        NativeOdb::open(dir.path()).unwrap(); // create the store up front

        const WRITERS: usize = 6;
        const UNIQUE: usize = 20;
        const SHARED: usize = 5;
        let barrier = Arc::new(Barrier::new(WRITERS));
        let path = dir.path().to_path_buf();

        // content helpers shared by writers and the verifier
        let unique = |w: usize, i: usize| format!("w{w}-obj{i}").into_bytes();
        let shared = |i: usize| format!("shared-obj{i}").into_bytes();
        let oid = |data: &[u8]| ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, data);

        let mut handles = Vec::new();
        for w in 0..WRITERS {
            let barrier = Arc::clone(&barrier);
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                // each thread is its own opener → its own flock description,
                // exactly like separate processes contending on the store.
                let mut odb = NativeOdb::open(&path).unwrap();
                barrier.wait();
                // interleave unique and shared puts so writers race on both
                // appends and dedup of the same content
                for i in 0..UNIQUE {
                    let d = unique(w, i);
                    odb.put(oid(&d), ObjectKind::Blob, &d).unwrap();
                    if i < SHARED {
                        let s = shared(i);
                        odb.put(oid(&s), ObjectKind::Blob, &s).unwrap();
                    }
                }
                odb.flush().unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // reopen and verify: every object present with the right bytes, and
        // shared content deduped to exactly one map entry (no torn appends).
        let odb = NativeOdb::open(&path).unwrap();
        for w in 0..WRITERS {
            for i in 0..UNIQUE {
                let d = unique(w, i);
                let got = odb.get(&oid(&d)).unwrap().expect("unique object present");
                assert_eq!(got.data, d, "w{w} obj{i}");
            }
        }
        for i in 0..SHARED {
            let s = shared(i);
            let got = odb.get(&oid(&s)).unwrap().expect("shared object present");
            assert_eq!(got.data, s);
        }
        assert_eq!(
            odb.len().unwrap(),
            WRITERS * UNIQUE + SHARED,
            "shared content stored once; no duplicate map records"
        );
    }

    #[test]
    fn deflate_blob_round_trips_through_tier1_after_reopen() {
        // A blob whose bytes happen to be a real libz-deflated stream
        // (e.g. a `.png` IDAT payload, a zip member, a raw git loose
        // object) must round-trip through the prism: put → decompose →
        // store parts + record → reopen → get → recompose → byte-equal.
        // Uses the deflate prism's own recompose to synthesise the
        // libz-shaped stream without a libz dev-dep.
        let dir = alt_os::temp::tempdir().unwrap();
        // a payload long enough to survive deflate's overhead; non-trivial
        // content so dedup is meaningful for follow-on assets
        let payload = b"hello world, this is a binary asset wrapped in zlib".repeat(8);
        let stream = alt_prism_deflate::DeflatePrism
            .recompose(&[1], &[&payload])
            .expect("synthesise level-1 libz stream");
        assert_ne!(stream, payload, "stream must differ from inflated payload");
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &stream);
        let alt = {
            let mut odb = NativeOdb::open(dir.path()).unwrap();
            let id = odb.put(oid, ObjectKind::Blob, &stream).unwrap();
            odb.flush().unwrap();
            id
        };
        // reopen: tier1 file must have made the recipe durable; recompose
        // returns the original stream bytes byte-for-byte.
        let odb = NativeOdb::open(dir.path()).unwrap();
        let back = odb.get(&oid).unwrap().expect("blob present after reopen");
        assert_eq!(back.kind, ObjectKind::Blob);
        assert_eq!(back.data, stream, "tier1 recompose must be byte-exact");
        assert_eq!(odb.lookup(&oid).unwrap().unwrap().alt, alt);
    }

    #[test]
    fn for_each_object_emits_tier1_blobs_recomposed() {
        // The export / full-clone bulk read goes through
        // for_each_object_unverified; it must emit Tier 1 blobs (whose
        // `alt` id is not actually stored in the chunk store) too, or the
        // exported pack ends up with fewer objects than its declared
        // header count.
        let dir = alt_os::temp::tempdir().unwrap();
        let payload = b"asset bytes wrapped in libz".repeat(6);
        let stream = alt_prism_deflate::DeflatePrism
            .recompose(&[1], &[&payload])
            .unwrap();
        let plain = b"plain text, not a prism candidate\n".to_vec();

        let (oid_stream, oid_plain) = {
            let mut odb = NativeOdb::open(dir.path()).unwrap();
            let s = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &stream);
            let p = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &plain);
            odb.put(s, ObjectKind::Blob, &stream).unwrap();
            odb.put(p, ObjectKind::Blob, &plain).unwrap();
            odb.flush().unwrap();
            (s, p)
        };

        let odb = NativeOdb::open(dir.path()).unwrap();
        let mut seen: Vec<(ObjectId, Vec<u8>)> = Vec::new();
        odb.for_each_object_unverified(|entry, bytes| {
            seen.push((entry.git, bytes.to_vec()));
        })
        .unwrap();
        seen.sort_by_key(|(id, _)| *id);
        let mut expected = vec![(oid_stream, stream.clone()), (oid_plain, plain.clone())];
        expected.sort_by_key(|(id, _)| *id);
        assert_eq!(
            seen, expected,
            "Tier 1 + Tier 0 both flow through bulk read"
        );
    }

    #[test]
    fn non_prism_blob_falls_through_tier0_unchanged() {
        // Anything that isn't a libz stream (text, random bytes) must
        // fall through to Tier 0 unchanged — no spurious recipe entries.
        let dir = alt_os::temp::tempdir().unwrap();
        let data = b"plain text, definitely not a zlib stream\n".repeat(4);
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data);
        {
            let mut odb = NativeOdb::open(dir.path()).unwrap();
            odb.put(oid, ObjectKind::Blob, &data).unwrap();
            odb.flush().unwrap();
        }
        let odb = NativeOdb::open(dir.path()).unwrap();
        let back = odb.get(&oid).unwrap().expect("present");
        assert_eq!(back.data, data);
    }

    #[test]
    fn concurrent_writers_through_pack_seals_and_rolls() {
        use std::sync::{Arc, Barrier};

        let dir = alt_os::temp::tempdir().unwrap();
        // a tiny seal threshold so writers roll packs constantly, exercising
        // the seq-set-change reload and the seal-roll create-race fallback
        let mut opts = BlobOptions::default();
        opts.chunks.seal_threshold = 1024;
        NativeOdb::open_with(dir.path(), opts).unwrap();

        const WRITERS: usize = 4;
        const PER: usize = 40;
        let barrier = Arc::new(Barrier::new(WRITERS));
        let path = dir.path().to_path_buf();

        // incompressible 200-byte content (stored raw), unique per (w, i)
        let content = |w: usize, i: usize| {
            let mut s = (w as u64)
                .wrapping_mul(1_000_003)
                .wrapping_add(i as u64 + 1);
            let mut v = Vec::with_capacity(200);
            while v.len() < 200 {
                s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut z = s;
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                v.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
            }
            v.truncate(200);
            v
        };
        let oid = |d: &[u8]| ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, d);

        let mut handles = Vec::new();
        for w in 0..WRITERS {
            let barrier = Arc::clone(&barrier);
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                let mut odb = NativeOdb::open_with(&path, opts).unwrap();
                barrier.wait();
                for i in 0..PER {
                    let d = content(w, i);
                    odb.put(oid(&d), ObjectKind::Blob, &d).unwrap();
                    // flush mid-stream to release + re-acquire (more catch-ups)
                    if i % 7 == 0 {
                        odb.flush().unwrap();
                    }
                }
                odb.flush().unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let odb = NativeOdb::open_with(&path, opts).unwrap();
        for w in 0..WRITERS {
            for i in 0..PER {
                let d = content(w, i);
                assert_eq!(
                    odb.get(&oid(&d)).unwrap().expect("present").data,
                    d,
                    "w{w} obj{i} survives concurrent seals"
                );
            }
        }
        assert_eq!(odb.len().unwrap(), WRITERS * PER);
    }

    #[test]
    fn checkpoint_then_rewind_restores_pre_batch_state() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let (a_oid, _) = put_one(&mut odb, HashAlgo::Sha1, ObjectKind::Blob, b"alpha");
        let (b_oid, _) = put_one(&mut odb, HashAlgo::Sha256, ObjectKind::Commit, b"beta");
        odb.flush().unwrap();
        let pre_len = odb.len().unwrap();
        let ckpt = odb.checkpoint().unwrap();
        let (c_oid, _) = put_one(&mut odb, HashAlgo::Sha1, ObjectKind::Blob, b"gamma");
        let (d_oid, _) = put_one(&mut odb, HashAlgo::Sha1, ObjectKind::Tree, b"delta");
        assert!(odb.contains(&c_oid).unwrap());
        assert!(odb.contains(&d_oid).unwrap());
        assert_eq!(odb.len().unwrap(), pre_len + 2);
        odb.rewind(ckpt).unwrap();
        assert!(
            odb.contains(&a_oid).unwrap(),
            "pre-checkpoint object survives"
        );
        assert!(
            odb.contains(&b_oid).unwrap(),
            "pre-checkpoint object survives"
        );
        assert!(
            !odb.contains(&c_oid).unwrap(),
            "post-checkpoint object is gone"
        );
        assert!(
            !odb.contains(&d_oid).unwrap(),
            "post-checkpoint object is gone"
        );
        assert_eq!(odb.len().unwrap(), pre_len);
        // close + re-open: on-disk state matches the rewound view
        drop(odb);
        let odb = NativeOdb::open(dir.path()).unwrap();
        assert!(odb.contains(&a_oid).unwrap());
        assert!(odb.contains(&b_oid).unwrap());
        assert!(!odb.contains(&c_oid).unwrap());
        assert!(!odb.contains(&d_oid).unwrap());
        assert_eq!(odb.len().unwrap(), pre_len);
    }

    #[test]
    fn rewind_to_fresh_checkpoint_empties_the_odb() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let ckpt = odb.checkpoint().unwrap();
        let (oid, _) = put_one(&mut odb, HashAlgo::Sha1, ObjectKind::Blob, b"orphan");
        assert!(odb.contains(&oid).unwrap());
        odb.rewind(ckpt).unwrap();
        assert!(!odb.contains(&oid).unwrap());
        assert_eq!(odb.len().unwrap(), 0);
        drop(odb);
        let odb = NativeOdb::open(dir.path()).unwrap();
        assert!(!odb.contains(&oid).unwrap());
        assert_eq!(odb.len().unwrap(), 0);
    }

    #[test]
    fn rewind_preserves_alt_aliases_for_kept_entries() {
        // an alt blob can back several git oids (same content, different kind).
        // rewinding past one alias must leave the surviving alias intact.
        let dir = alt_os::temp::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let data = b"";
        let (blob_oid, alt) = put_one(&mut odb, HashAlgo::Sha1, ObjectKind::Blob, data);
        let ckpt = odb.checkpoint().unwrap();
        let (tree_oid, alt2) = put_one(&mut odb, HashAlgo::Sha1, ObjectKind::Tree, data);
        assert_eq!(alt, alt2);
        assert_eq!(odb.lookup_by_alt(alt).unwrap().len(), 2);
        odb.rewind(ckpt).unwrap();
        assert!(odb.contains(&blob_oid).unwrap());
        assert!(!odb.contains(&tree_oid).unwrap());
        assert_eq!(
            odb.lookup_by_alt(alt).unwrap().len(),
            1,
            "the surviving alias is still indexed by alt"
        );
    }
}
