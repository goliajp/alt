//! Corpus sweep: every blob of every repo under `$ALT_CORPUS` goes through
//! the blob store and back, byte-identical. All repos share one store, so
//! the dedup/volume counters at the end are the cross-repo account.

use std::fs;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectKind};
use alt_git_pack::IndexedPack;
use alt_store::BlobStore;

/// Feeds every blob in the repo (packed and loose) to `f`.
fn for_each_blob(repo: &Path, mut f: impl FnMut(&[u8])) {
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
                for (offset, _) in order {
                    let obj = indexed.read_at(offset).unwrap();
                    if obj.kind == ObjectKind::Blob {
                        f(&obj.data);
                    }
                }
            }
        }
    }
    alt_testutil::for_each_loose(repo, |_oid, raw| {
        if raw.kind == ObjectKind::Blob {
            f(&raw.data);
        }
    });
}

#[test]
#[ignore = "needs $ALT_CORPUS pointing at a directory of git repos"]
fn corpus_blobs_round_trip_through_blob_store() {
    let corpus = std::env::var("ALT_CORPUS").expect("set ALT_CORPUS to the corpus directory");
    let store_dir = tempfile::tempdir().unwrap();
    let mut store = BlobStore::open(store_dir.path()).unwrap();

    let mut blobs = 0u64;
    for entry in fs::read_dir(&corpus).unwrap() {
        let repo = entry.unwrap().path();
        if !repo.join(".git").is_dir() {
            continue;
        }
        let mut repo_blobs = 0u64;
        for_each_blob(&repo, |data| {
            let id = store.put(data).unwrap();
            let back = store.get(id).unwrap();
            assert_eq!(back, data, "round-trip mismatch for blob {id} in {repo:?}");
            repo_blobs += 1;
        });
        blobs += repo_blobs;
        eprintln!("{repo:?}: {repo_blobs} blobs");
    }
    store.flush().unwrap();
    assert!(blobs > 0, "corpus must contain blobs");

    let c = store.counters();
    let disk: u64 = fs::read_dir(store_dir.path())
        .unwrap()
        .map(|e| e.unwrap().metadata().unwrap().len())
        .sum();
    eprintln!(
        "corpus blob account: {blobs} blobs, {} chunk puts ({} dedup hits), \
         {} bytes in, {} bytes written, {} bytes on disk (packs)",
        c.puts, c.dedup_hits, c.bytes_in, c.bytes_written, disk
    );
    assert!(c.dedup_hits > 0, "a real corpus must dedup something");
}
