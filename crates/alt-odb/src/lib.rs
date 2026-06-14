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

mod map;

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use alt_git_codec::{ObjectId, ObjectKind, RawObject};
use alt_store::{BlobId, BlobOptions, BlobStore, CompactReport, StoreError};

pub use map::{MapEntry, ObjectMap};

/// Takes an exclusive advisory lock on the odb write-lock file for the duration
/// of one write batch. `flock` is per-open-file-description and auto-releases on
/// close/crash, so there are no stale locks. Non-unix has no cross-process lock
/// yet — single-writer there (documented limitation, like the op log).
#[cfg(unix)]
fn lock_exclusive(file: &File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Shared (read) lock — taken while opening, so the open's reads + crash
/// recovery of the append files can't overlap an exclusive writer's append.
/// Multiple opens share it; an exclusive writer waits for them and vice versa.
#[cfg(unix)]
fn lock_shared(file: &File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn unlock(file: &File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn lock_shared(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn lock_exclusive(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn unlock(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum OdbError {
    #[error("io")]
    Io(#[from] std::io::Error),
    #[error("store")]
    Store(#[from] StoreError),
    #[error("odb format: {0}")]
    Format(&'static str),
    #[error("object {claimed} does not hash to its id (got {actual})")]
    HashMismatch { claimed: ObjectId, actual: ObjectId },
    #[error("object {0} maps to {1} bytes but the store returned a different length")]
    SizeMismatch(ObjectId, u64),
}

/// The native object database: blob store + git-identity map.
///
/// Writes (`put`/`lineage_delta`/`compact`) are serialized across processes by
/// an exclusive `flock` on `odb.lock`, held from the first write of a batch
/// until `flush`. On acquiring it, the in-memory state is reconciled with
/// whatever other writers appended (`sync_from_disk`), so this batch dedups and
/// appends against the true current store. Reads take no lock.
pub struct NativeOdb {
    blobs: BlobStore,
    map: ObjectMap,
    /// The advisory write lock; `held` tracks whether this batch owns it.
    lock: File,
    held: bool,
    /// Group commit: a separate lock around the fsync, and a marker recording
    /// how far each append file is durable. A batch fsyncs only if no other
    /// writer already made its appends durable — so N concurrent commits
    /// coalesce to ~1 fsync (durability stops being per-commit, matching the
    /// no-fsync throughput while staying durable).
    sync_lock: File,
    durable_path: PathBuf,
}

/// The 3 durable EOFs (active pack, blobmap, map.alt) from the marker file;
/// all-zero when missing (everything must be re-fsynced).
fn read_durable(path: &Path) -> [u64; 3] {
    match std::fs::read(path) {
        Ok(b) if b.len() >= 24 => [
            u64::from_le_bytes(b[0..8].try_into().unwrap()),
            u64::from_le_bytes(b[8..16].try_into().unwrap()),
            u64::from_le_bytes(b[16..24].try_into().unwrap()),
        ],
        _ => [0; 3],
    }
}

/// Atomically records the durable EOFs (temp + rename) so a lock-free reader
/// sees the old or new value, never a torn one. Written only after the fsync,
/// so the marker never claims durability that has not happened.
fn write_durable(path: &Path, eofs: [u64; 3]) -> std::io::Result<()> {
    let mut buf = [0u8; 24];
    buf[0..8].copy_from_slice(&eofs[0].to_le_bytes());
    buf[8..16].copy_from_slice(&eofs[1].to_le_bytes());
    buf[16..24].copy_from_slice(&eofs[2].to_le_bytes());
    let tmp = path.with_extension("durable.tmp");
    std::fs::write(&tmp, buf)?;
    std::fs::rename(&tmp, path)
}

fn covers(durable: [u64; 3], target: [u64; 3]) -> bool {
    durable[0] >= target[0] && durable[1] >= target[1] && durable[2] >= target[2]
}

impl NativeOdb {
    /// Opens (or creates) the database under the `.alt` root directory.
    pub fn open(alt_dir: impl Into<PathBuf>) -> Result<Self, OdbError> {
        Self::open_with(alt_dir, BlobOptions::default())
    }

    pub fn open_with(alt_dir: impl Into<PathBuf>, opts: BlobOptions) -> Result<Self, OdbError> {
        let alt_dir = alt_dir.into();
        std::fs::create_dir_all(&alt_dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(alt_dir.join("odb.lock"))?;
        // Open the append files (active pack, blobmap, map.alt — read and
        // crash-recovered here) under a shared lock, so the open never overlaps
        // another process's exclusive append. Without this, an open scanning a
        // writer's half-written record truncates it and corrupts the store.
        lock_shared(&lock)?;
        let opened = (|| -> Result<(BlobStore, ObjectMap), OdbError> {
            let blobs = BlobStore::open_with(alt_dir.join("store"), opts)?;
            let map = ObjectMap::open(&alt_dir.join("map.alt"))?;
            Ok((blobs, map))
        })();
        let _ = unlock(&lock);
        let (blobs, map) = opened?;
        let sync_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(alt_dir.join("odb.sync.lock"))?;
        Ok(Self {
            blobs,
            map,
            lock,
            held: false,
            sync_lock,
            durable_path: alt_dir.join("odb.durable"),
        })
    }

    /// Acquires the write lock for this batch (if not already held) and brings
    /// the in-memory state up to date with concurrent writers.
    fn acquire(&mut self) -> Result<(), OdbError> {
        if self.held {
            return Ok(());
        }
        lock_exclusive(&self.lock)?;
        self.held = true;
        self.blobs.sync_from_disk()?;
        self.map.sync_from_disk()?;
        Ok(())
    }

    /// Releases the write lock (best-effort; the fd closing also releases it).
    fn release(&mut self) {
        if self.held {
            let _ = unlock(&self.lock);
            self.held = false;
        }
    }

    /// Stores one git object's canonical payload and records its identity
    /// bridge. The payload is re-hashed against `oid` — a wrong claimed id
    /// is rejected here rather than discovered at export. Idempotent.
    pub fn put(
        &mut self,
        oid: ObjectId,
        kind: ObjectKind,
        data: &[u8],
    ) -> Result<BlobId, OdbError> {
        self.acquire()?;
        // dedup against the now-current map (post-catch-up): a concurrent
        // writer may already have stored this object
        if let Some(entry) = self.map.by_git(&oid) {
            return Ok(entry.alt);
        }
        let actual = ObjectId::hash_object(oid.algo(), kind, data);
        if actual != oid {
            return Err(OdbError::HashMismatch {
                claimed: oid,
                actual,
            });
        }
        let alt = self.blobs.put(data)?;
        self.map.append(MapEntry {
            git: oid,
            alt,
            kind,
            size: data.len() as u64,
        })?;
        Ok(alt)
    }

    /// Reads an object back by git id, materializing its canonical payload.
    pub fn get(&self, oid: &ObjectId) -> Result<Option<RawObject>, OdbError> {
        let Some(entry) = self.map.by_git(oid) else {
            return Ok(None);
        };
        let data = self.blobs.get(entry.alt)?;
        if data.len() as u64 != entry.size {
            return Err(OdbError::SizeMismatch(*oid, entry.size));
        }
        Ok(Some(RawObject {
            kind: entry.kind,
            data,
        }))
    }

    /// Identity/kind/size lookup without materializing the payload
    /// (`cat-file -t` / `-s` class queries).
    pub fn lookup(&self, oid: &ObjectId) -> Option<&MapEntry> {
        self.map.by_git(oid)
    }

    /// The git identities sharing one stored content (usually one; can be
    /// several across kinds or hash algorithms).
    pub fn lookup_by_alt(&self, id: BlobId) -> impl Iterator<Item = &MapEntry> {
        self.map.by_alt(id)
    }

    pub fn contains(&self, oid: &ObjectId) -> bool {
        self.map.by_git(oid).is_some()
    }

    /// All mapped objects in import order (full-iteration basis for export
    /// and verification sweeps).
    pub fn entries(&self) -> impl Iterator<Item = &MapEntry> {
        self.map.iter()
    }

    /// Bulk-materializes every mapped object's bytes exactly once, without
    /// per-object re-hashing — the read path for export / full-clone serving.
    /// Single-chunk objects (the lineage-delta'd ones) go through the chunk
    /// store's decode-once forest, so a base shared by many versions is
    /// decoded just once; multi-chunk blobs (not delta'd) are assembled
    /// normally. Integrity belongs to the output boundary (export →
    /// `git fsck`), not to each read.
    pub fn for_each_object_unverified(
        &self,
        mut f: impl FnMut(&MapEntry, &[u8]),
    ) -> Result<(), OdbError> {
        self.blobs
            .chunk_store()
            .for_each_decoded(|chunk_id, bytes| {
                for entry in self.map.by_alt(BlobId(chunk_id.0)) {
                    f(entry, bytes);
                }
            })?;
        for entry in self.map.iter() {
            if self.blobs.is_multi_chunk(entry.alt) {
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
        let (Some(old_entry), Some(new_entry)) = (self.map.by_git(old), self.map.by_git(new))
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

    /// Durability point: blob store first (chunks before blobmap inside),
    /// then `map.alt`, so a crash never leaves a durable identity record
    /// pointing at lost content.
    pub fn flush(&mut self) -> Result<(), OdbError> {
        // my durability target = how far my appends reached in each file
        let (pack, blobmap) = self.blobs.appended_lens();
        let target = [pack, blobmap, self.map.appended_len()];
        // release the exclusive append lock first, so other writers append (and
        // pile onto the same fsync) instead of waiting behind my durability
        self.release();

        if alt_store::relaxed_durability() {
            return Ok(());
        }
        // fast path: another writer already fsynced past my appends
        if covers(read_durable(&self.durable_path), target) {
            return Ok(());
        }
        // else become the syncer for this batch: one fsync covers my appends and
        // any concurrent ones, then the marker lets them all skip their fsync
        lock_exclusive(&self.sync_lock)?;
        let result = (|| -> Result<(), OdbError> {
            if covers(read_durable(&self.durable_path), target) {
                return Ok(());
            }
            let (pf, bf) = self.blobs.file_lens()?;
            let eofs = [pf, bf, self.map.file_len()?];
            self.blobs.fsync()?; // chunks then blobmap, in order
            self.map.fsync()?; // then map.alt
            write_durable(&self.durable_path, eofs)?;
            Ok(())
        })();
        let _ = unlock(&self.sync_lock);
        result
    }
}

impl Drop for NativeOdb {
    fn drop(&mut self) {
        // release the write lock if a batch never reached flush
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_git_codec::HashAlgo;

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
        let dir = tempfile::tempdir().unwrap();
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
                let entry = odb.lookup(&oid).unwrap();
                assert_eq!(entry.size, data.len() as u64);
                assert_eq!(entry.alt, alt);
                assert!(odb.lookup_by_alt(alt).any(|e| e.git == oid));
            }
        }
        assert_eq!(odb.len(), 8);
    }

    #[test]
    fn wrong_claimed_oid_is_rejected_at_write() {
        let dir = tempfile::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"other content");
        let err = odb
            .put(oid, ObjectKind::Blob, b"actual content")
            .unwrap_err();
        assert!(matches!(err, OdbError::HashMismatch { .. }));
        assert!(!odb.contains(&oid), "a rejected put must record nothing");
    }

    #[test]
    fn empty_tree_is_a_first_class_object() {
        // native trees may be empty (empty-directory model lands here);
        // export degrades per git semantics, storage does not special-case
        let dir = tempfile::tempdir().unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let data = b"";
        let (blob_oid, blob_alt) = put_one(&mut odb, HashAlgo::Sha1, ObjectKind::Blob, data);
        let (tree_oid, tree_alt) = put_one(&mut odb, HashAlgo::Sha1, ObjectKind::Tree, data);
        assert_ne!(blob_oid, tree_oid, "git ids differ via the header");
        assert_eq!(blob_alt, tree_alt, "content is stored exactly once");
        assert_eq!(odb.get(&blob_oid).unwrap().unwrap().kind, ObjectKind::Blob);
        assert_eq!(odb.get(&tree_oid).unwrap().unwrap().kind, ObjectKind::Tree);
        assert_eq!(odb.lookup_by_alt(blob_alt).count(), 2);
    }

    #[test]
    fn duplicate_put_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let (oid, a) = put_one(&mut odb, HashAlgo::Sha1, ObjectKind::Blob, b"dup");
        let b = odb.put(oid, ObjectKind::Blob, b"dup").unwrap();
        assert_eq!(a, b);
        assert_eq!(odb.len(), 1);
    }

    #[test]
    fn missing_object_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let odb = NativeOdb::open(dir.path()).unwrap();
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"absent");
        assert!(odb.get(&oid).unwrap().is_none());
        assert!(odb.lookup(&oid).is_none());
    }

    #[test]
    fn concurrent_writers_store_everything_without_corruption() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
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
            odb.len(),
            WRITERS * UNIQUE + SHARED,
            "shared content stored once; no duplicate map records"
        );
    }

    #[test]
    fn concurrent_writers_through_pack_seals_and_rolls() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
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
        assert_eq!(odb.len(), WRITERS * PER);
    }
}
