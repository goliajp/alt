//! Compares Index against `git ls-files --stage -z` across index versions
//! 2, 3 and 4, including conflict stages and skip-worktree.

use std::path::Path;
use std::process::Command;

use alt_git_codec::HashAlgo;
use alt_git_index::Index;
use alt_testutil as common;

/// Renders our entries in `ls-files --stage` shape:
/// `<mode> <oid> <stage>\t<path>`.
fn ours(repo: &Path, algo: HashAlgo) -> Vec<String> {
    let index = Index::open(&repo.join(".git/index"), algo).unwrap();
    index
        .entries
        .iter()
        .map(|e| format!("{:06o} {} {}\t{}", e.mode, e.oid, e.stage(), e.path))
        .collect()
}

fn git_stage_list(repo: &Path) -> Vec<String> {
    common::git(repo, &["ls-files", "--stage", "-z"])
        .split_terminator('\0')
        .map(str::to_owned)
        .collect()
}

fn assert_matches(repo: &Path, algo: HashAlgo) -> Index {
    assert_eq!(
        ours(repo, algo),
        git_stage_list(repo),
        "vs ls-files --stage"
    );
    Index::open(&repo.join(".git/index"), algo).unwrap()
}

fn index_matches_git(algo: HashAlgo, object_format: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    common::make_repo(repo, object_format);

    // v2 (default for plain repos)
    let index = assert_matches(repo, algo);
    assert_eq!(index.version, 2);
    assert!(
        index.extensions.iter().any(|e| &e.signature == b"TREE"),
        "fixture index should carry a cache-tree extension"
    );

    // v3: skip-worktree sets an extended flag
    common::git(repo, &["update-index", "--skip-worktree", "a.txt"]);
    let index = assert_matches(repo, algo);
    assert_eq!(index.version, 3);
    let entry = index
        .entries
        .iter()
        .find(|e| e.path == "a.txt")
        .expect("a.txt is tracked");
    assert!(entry.skip_worktree());
    common::git(repo, &["update-index", "--no-skip-worktree", "a.txt"]);

    // v4: prefix-compressed paths
    common::git(repo, &["update-index", "--index-version", "4"]);
    let index = assert_matches(repo, algo);
    assert_eq!(index.version, 4);

    // conflict: stages 1/2/3 appear
    common::git(repo, &["checkout", "-q", "-b", "clash", "HEAD~2"]);
    std::fs::write(repo.join("a.txt"), "clash side\n").unwrap();
    common::git(repo, &["commit", "-q", "-am", "clash side"]);
    let merge = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge", "main"])
        .output()
        .unwrap();
    assert!(!merge.status.success(), "merge must conflict");
    let index = assert_matches(repo, algo);
    assert!(
        index.entries.iter().any(|e| e.stage() != 0),
        "conflicted index must contain non-zero stages"
    );
}

#[test]
fn serialize_round_trips_and_git_reads_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    common::make_repo(repo, "sha1");
    let algo = HashAlgo::Sha1;

    let before = git_stage_list(repo);
    let index = Index::open(&repo.join(".git/index"), algo).unwrap();

    // parse → serialize → parse reproduces the entries (and the TREE ext)
    let bytes = index.serialize(algo);
    let reparsed = Index::parse(&bytes, algo).unwrap();
    assert_eq!(reparsed.entries, index.entries, "entries round-trip");
    assert_eq!(
        reparsed.extensions, index.extensions,
        "extensions preserved"
    );

    // and git itself reads our serialized index identically
    std::fs::write(repo.join(".git/index"), &bytes).unwrap();
    assert_eq!(
        git_stage_list(repo),
        before,
        "git must read our serialized index the same"
    );
}

#[test]
fn index_matches_git_sha1() {
    index_matches_git(HashAlgo::Sha1, "sha1");
}

#[test]
fn index_matches_git_sha256() {
    index_matches_git(HashAlgo::Sha256, "sha256");
}
