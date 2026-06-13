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

use std::path::PathBuf;

use alt_git_codec::{ObjectId, ObjectKind, RawObject};
use alt_store::{BlobId, BlobOptions, BlobStore, CompactReport, StoreError};

pub use map::{MapEntry, ObjectMap};

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
pub struct NativeOdb {
    blobs: BlobStore,
    map: ObjectMap,
}

impl NativeOdb {
    /// Opens (or creates) the database under the `.alt` root directory.
    pub fn open(alt_dir: impl Into<PathBuf>) -> Result<Self, OdbError> {
        Self::open_with(alt_dir, BlobOptions::default())
    }

    pub fn open_with(alt_dir: impl Into<PathBuf>, opts: BlobOptions) -> Result<Self, OdbError> {
        let alt_dir = alt_dir.into();
        std::fs::create_dir_all(&alt_dir)?;
        let blobs = BlobStore::open_with(alt_dir.join("store"), opts)?;
        let map = ObjectMap::open(&alt_dir.join("map.alt"))?;
        Ok(Self { blobs, map })
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

    /// Re-encodes `old`'s content as a lineage delta against `new`'s
    /// (same-path predecessor → successor). Identity is untouched; only
    /// the storage form changes. Returns whether a re-encoding happened.
    pub fn lineage_delta(&mut self, old: &ObjectId, new: &ObjectId) -> Result<bool, OdbError> {
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
        self.blobs.flush()?;
        self.map.sync()
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
}
