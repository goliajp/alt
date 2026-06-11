//! Packs a real git repository and verifies idx lookup and non-delta
//! decoding against re-hashing. Delta entries are counted and skipped
//! until S5.

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

        let info = pack.entry_info(idx.offset_at(i).unwrap()).unwrap();
        match info.kind {
            EntryKind::Plain(kind) => {
                let data = pack.inflate(info.data_at, info.size).unwrap();
                assert_eq!(
                    ObjectId::hash_object(algo, kind, &data),
                    oid,
                    "re-hash mismatch for {oid}"
                );
                plain += 1;
            }
            EntryKind::OfsDelta { base_at } => {
                // base must be a parseable entry earlier in the pack
                pack.entry_info(base_at).unwrap();
                delta += 1;
            }
            EntryKind::RefDelta { base } => {
                assert!(
                    idx.lookup(&base).is_some(),
                    "ref-delta base must be in pack"
                );
                delta += 1;
            }
        }
    }
    assert!(plain >= 10, "expected mostly plain entries, got {plain}");
    println!("{object_format}: {plain} plain + {delta} delta entries verified");

    let absent = ObjectId::hash_object(algo, alt_git_codec::ObjectKind::Blob, b"not in pack");
    assert_eq!(idx.lookup(&absent), None);
}

#[test]
fn verifies_sha1_pack() {
    verify_pack(HashAlgo::Sha1, "sha1");
}

#[test]
fn verifies_sha256_pack() {
    verify_pack(HashAlgo::Sha256, "sha256");
}
