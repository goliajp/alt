//! Corpus sweep over real packfiles: resolves every entry of every pack
//! under `$ALT_CORPUS` and re-hashes it against the idx oid.

use std::fs;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId};
use alt_git_pack::{EntryKind, IndexedPack};

fn sweep_pack(pack_path: &Path) -> (u32, u32) {
    // corpus repos are SHA-1 until S9 wires config detection
    let indexed = IndexedPack::open(pack_path, HashAlgo::Sha1).unwrap();
    let idx = indexed.idx();

    // ascending pack offset: bases come before their deltas, which is the
    // cache-friendly order the verify harness will also use
    let mut order: Vec<(u64, u32)> = (0..idx.len())
        .map(|i| (idx.offset_at(i).unwrap(), i))
        .collect();
    order.sort_unstable();

    let (mut plain, mut delta) = (0u32, 0u32);
    for (offset, i) in order {
        let oid = idx.oid_at(i);
        match indexed.pack().entry_info(offset).unwrap().kind {
            EntryKind::Plain(_) => plain += 1,
            _ => delta += 1,
        }
        let obj = indexed.read_at(offset).unwrap();
        assert_eq!(
            ObjectId::hash_object(HashAlgo::Sha1, obj.kind, &obj.data),
            oid,
            "re-hash mismatch for {oid} in {pack_path:?}"
        );
    }
    (plain, delta)
}

#[test]
#[ignore = "needs $ALT_CORPUS pointing at a directory of git repos"]
fn sweep_corpus_packs() {
    let corpus = std::env::var("ALT_CORPUS").expect("set ALT_CORPUS to the corpus directory");
    let mut total_delta = 0;
    for entry in fs::read_dir(&corpus).unwrap() {
        let repo = entry.unwrap().path();
        let pack_dir = repo.join(".git/objects/pack");
        if !pack_dir.is_dir() {
            continue;
        }
        for file in fs::read_dir(&pack_dir).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_some_and(|e| e == "pack") {
                let (plain, delta) = sweep_pack(&path);
                total_delta += delta;
                println!(
                    "{}: {plain} plain + {delta} delta entries verified",
                    path.display()
                );
            }
        }
    }
    assert!(total_delta > 0, "corpus should exercise real delta chains");
}
