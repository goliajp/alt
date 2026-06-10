//! Corpus sweep: for every repository under `$ALT_CORPUS`, parse, re-serialize
//! and re-hash every loose object. Run explicitly:
//!
//! ```sh
//! ALT_CORPUS=.claude/corpus cargo test -p alt-git-codec --test corpus_sweep -- --ignored
//! ```

mod common;

use std::path::Path;

use alt_git_codec::{Commit, ObjectId, ObjectKind, Tag, Tree};

fn sweep_repo(repo: &Path) -> (usize, usize) {
    let mut non_blob = 0;
    let total = common::for_each_loose(repo, |oid, raw| {
        let algo = oid.algo();
        assert_eq!(
            ObjectId::hash_object(algo, raw.kind, &raw.data),
            oid,
            "re-hash mismatch for {oid} in {repo:?}"
        );
        let reserialized = match raw.kind {
            ObjectKind::Blob => return,
            ObjectKind::Commit => Commit::parse(&raw.data).unwrap().serialize(),
            ObjectKind::Tree => Tree::parse(&raw.data, algo).unwrap().serialize(),
            ObjectKind::Tag => Tag::parse(&raw.data).unwrap().serialize(),
        };
        assert_eq!(reserialized, raw.data, "round-trip mismatch for {oid} in {repo:?}");
        non_blob += 1;
    });
    (total, non_blob)
}

#[test]
#[ignore = "needs $ALT_CORPUS pointing at a directory of git repos"]
fn sweep_corpus() {
    let corpus = std::env::var("ALT_CORPUS").expect("set ALT_CORPUS to the corpus directory");
    let mut swept = 0;
    for entry in std::fs::read_dir(&corpus).unwrap() {
        let repo = entry.unwrap().path();
        if !repo.join(".git/objects").is_dir() {
            continue;
        }
        let (total, non_blob) = sweep_repo(&repo);
        println!(
            "{}: {total} loose objects verified ({non_blob} non-blob round-tripped)",
            repo.display()
        );
        // fully-packed repos legitimately have zero loose objects
        swept += total;
    }
    assert!(swept > 0, "no loose objects anywhere under {corpus}");
}
