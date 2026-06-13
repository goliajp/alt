//! Cross-check against git: flatten a HEAD tree to the same paths/oids as
//! `git ls-tree -r`, and report a freshly-committed tree as clean status.

use std::path::Path;

use alt_git_codec::HashAlgo;
use alt_odb::NativeOdb;
use alt_repo::Repository;
use alt_testutil as common;
use alt_worktree::{flatten_tree, index_entries, scan_worktree, status};

/// A controlled little repo: a file, a nested file, an executable.
fn make_small_repo(dir: &Path) {
    common::git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/b.txt"), "nested\n").unwrap();
    std::fs::write(dir.join("run.sh"), "#!/bin/sh\n").unwrap();
    common::git(dir, &["add", "."]);
    common::git(dir, &["update-index", "--chmod=+x", "run.sh"]);
    common::git(dir, &["commit", "-q", "-m", "init"]);
    common::git(dir, &["checkout", "-q", "."]); // make run.sh executable on disk
}

#[test]
fn flatten_matches_git_ls_tree_and_status_is_clean() {
    let src = tempfile::tempdir().unwrap();
    make_small_repo(src.path());

    let repo = Repository::discover(src.path()).unwrap();
    let algo = HashAlgo::Sha1;
    let alt_root = tempfile::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");
    alt_import::import_git(&repo, &alt_dir, "test/worktree", 1).unwrap();

    let head = repo.rev_parse("HEAD").unwrap().unwrap();
    let tree = repo.read_commit(&head).unwrap().tree().unwrap();

    let odb = NativeOdb::open(&alt_dir).unwrap();
    let flat = flatten_tree(&odb, tree, algo).unwrap();

    // git's own view: "<mode> blob <oid>\t<path>"
    let want = common::git(src.path(), &["ls-tree", "-r", "HEAD"]);
    let ours: String = flat
        .iter()
        .map(|e| format!("{:06o} blob {}\t{}\n", e.mode, e.oid, e.path))
        .collect();
    assert_eq!(ours, want, "flatten must match git ls-tree -r");

    // a freshly-committed tree: HEAD == index == worktree => clean status
    let worktree = scan_worktree(src.path(), algo).unwrap();
    let index = alt_git_index::Index::open(&src.path().join(".git/index"), algo).unwrap();
    let s = status(&flat, &index_entries(&index), &worktree);
    assert!(
        s.staged.is_empty() && s.unstaged.is_empty() && s.untracked.is_empty(),
        "a clean checkout has no changes, got {s:?}"
    );
}
