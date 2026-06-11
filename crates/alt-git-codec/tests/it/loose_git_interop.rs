//! Reads every loose object of real git repositories (SHA-1 and SHA-256)
//! and verifies each one by re-hashing.

use alt_git_codec::{HashAlgo, ObjectId};

use crate::common;

fn verify_all_loose(algo: HashAlgo, object_format: &str) {
    let tmp = tempfile::tempdir().unwrap();
    common::make_repo(tmp.path(), object_format);
    let n = common::for_each_loose(tmp.path(), |oid, raw| {
        assert_eq!(oid.algo(), algo);
        assert_eq!(
            ObjectId::hash_object(algo, raw.kind, &raw.data),
            oid,
            "re-hash mismatch for {oid}"
        );
    });
    // 8 blobs, >=6 trees, 5 commits, 1 tag
    assert!(n >= 15, "expected >= 15 loose objects, got {n}");
}

#[test]
fn reads_all_loose_objects_of_a_sha1_repo() {
    verify_all_loose(HashAlgo::Sha1, "sha1");
}

#[test]
fn reads_all_loose_objects_of_a_sha256_repo() {
    verify_all_loose(HashAlgo::Sha256, "sha256");
}
