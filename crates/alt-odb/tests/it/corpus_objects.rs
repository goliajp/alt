//! Corpus sweep: every object of every repo under `$ALT_CORPUS` (packed and
//! loose, all four kinds) goes through the native odb and back. Writes
//! re-hash against the git oid inside `put` (fidelity, read direction);
//! reads must return the canonical bytes exactly.

use std::fs;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, RawObject};
use alt_git_pack::IndexedPack;
use alt_odb::NativeOdb;

/// Feeds every object in the repo (packed and loose) to `f`.
fn for_each_object(repo: &Path, mut f: impl FnMut(ObjectId, RawObject)) {
    let pack_dir = repo.join(".git/objects/pack");
    if let Ok(entries) = fs::read_dir(&pack_dir) {
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "pack") {
                // corpus repos are SHA-1 (same assumption as the pack sweep)
                let indexed = IndexedPack::open(&path, HashAlgo::Sha1).unwrap();
                let idx = indexed.idx();
                let mut order: Vec<(u64, u32)> = (0..idx.len())
                    .map(|i| (idx.offset_at(i).unwrap(), i))
                    .collect();
                order.sort_unstable();
                for (offset, i) in order {
                    let obj = indexed.read_at(offset).unwrap();
                    f(idx.oid_at(i), obj.to_raw());
                }
            }
        }
    }
    alt_testutil::for_each_loose(repo, f);
}

#[test]
#[ignore = "needs $ALT_CORPUS pointing at a directory of git repos"]
fn corpus_objects_round_trip_through_native_odb() {
    let corpus = std::env::var("ALT_CORPUS").expect("set ALT_CORPUS to the corpus directory");
    let store_dir = tempfile::tempdir().unwrap();
    let mut odb = NativeOdb::open(store_dir.path()).unwrap();

    let mut objects = 0u64;
    for entry in fs::read_dir(&corpus).unwrap() {
        let repo = entry.unwrap().path();
        if !repo.join(".git").is_dir() {
            continue;
        }
        let mut repo_objects = 0u64;
        for_each_object(&repo, |oid, raw| {
            // put re-hashes canonical bytes against oid: a mismatch panics
            odb.put(oid, raw.kind, &raw.data).unwrap();
            let back = odb.get(&oid).unwrap().unwrap();
            assert_eq!(back.kind, raw.kind, "kind mismatch for {oid} in {repo:?}");
            assert_eq!(back.data, raw.data, "byte mismatch for {oid} in {repo:?}");
            repo_objects += 1;
        });
        objects += repo_objects;
        eprintln!("{repo:?}: {repo_objects} objects");
    }
    odb.flush().unwrap();
    assert!(objects > 0, "corpus must contain objects");

    let c = odb.blobs().counters();
    eprintln!(
        "corpus object account: {objects} objects ({} mapped), \
         {} chunk puts ({} dedup hits), {} bytes in, {} bytes written",
        odb.len(),
        c.puts,
        c.dedup_hits,
        c.bytes_in,
        c.bytes_written
    );
}
