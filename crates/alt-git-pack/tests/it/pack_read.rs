//! Packs a real git repository and verifies that every entry — plain and
//! delta — resolves and re-hashes to its idx oid.

use std::fs;
use std::path::{Path, PathBuf};

use alt_git_codec::{HashAlgo, ObjectId};
use alt_git_pack::{EntryKind, IndexedPack};
use alt_testutil as common;

fn packed_fixture(dir: &Path, object_format: &str) -> PathBuf {
    common::make_repo(dir, object_format);
    let pack_dir = common::pack_repo(dir);
    fs::read_dir(pack_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "pack"))
        .expect("repack must produce one pack")
}

fn verify_pack(algo: HashAlgo, object_format: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let pack_path = packed_fixture(tmp.path(), object_format);
    let indexed = IndexedPack::open(&pack_path, algo).unwrap();
    let (idx, pack) = (indexed.idx(), indexed.pack());

    assert_eq!(idx.len(), pack.object_count());
    assert!(idx.len() >= 15, "fixture should pack >= 15 objects");

    let (mut plain, mut delta) = (0u32, 0u32);
    for i in 0..idx.len() {
        let oid = idx.oid_at(i);
        assert_eq!(idx.lookup(&oid), Some(i), "lookup must invert oid_at");

        let offset = idx.offset_at(i).unwrap();
        match pack.entry_info(offset).unwrap().kind {
            EntryKind::Plain(_) => plain += 1,
            EntryKind::OfsDelta { .. } | EntryKind::RefDelta { .. } => delta += 1,
        }

        let obj = indexed.read(&oid).unwrap().expect("indexed oid must read");
        assert_eq!(
            ObjectId::hash_object(algo, obj.kind, &obj.data),
            oid,
            "re-hash mismatch for {oid}"
        );
    }
    assert!(plain >= 10, "expected plain entries, got {plain}");
    assert!(delta >= 1, "fixture should produce delta entries");
    println!("{object_format}: {plain} plain + {delta} delta entries verified");

    let absent = ObjectId::hash_object(algo, alt_git_codec::ObjectKind::Blob, b"not in pack");
    assert!(indexed.read(&absent).unwrap().is_none());
}

#[test]
fn verifies_sha1_pack() {
    verify_pack(HashAlgo::Sha1, "sha1");
}

#[test]
fn verifies_sha256_pack() {
    verify_pack(HashAlgo::Sha256, "sha256");
}
