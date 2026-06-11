//! Compares the reftable backend against `git for-each-ref` /
//! `git rev-parse`, including tombstones, stacked tables, compaction,
//! and multi-block tables.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use alt_git_codec::HashAlgo;
use alt_git_refs::{RefStore, RefTarget};
use alt_testutil as common;

fn git_refs(repo: &Path) -> Vec<(String, String)> {
    common::git(repo, &["for-each-ref", "--format=%(objectname) %(refname)"])
        .lines()
        .map(|l| {
            let (oid, name) = l.split_once(' ').unwrap();
            (oid.to_owned(), name.to_owned())
        })
        .collect()
}

fn assert_matches_git(repo: &Path, algo: HashAlgo) -> RefStore {
    let store = RefStore::open(repo.join(".git"), algo).unwrap();
    let ours: Vec<(String, String)> = store
        .iter_refs()
        .unwrap()
        .iter()
        .map(|r| {
            let resolved = match &r.target {
                RefTarget::Direct(oid) => oid.to_string(),
                RefTarget::Symbolic(name) => store
                    .resolve(&name.to_string())
                    .unwrap()
                    .expect("symref target must resolve")
                    .to_string(),
            };
            (resolved, r.name.to_string())
        })
        .collect();
    assert_eq!(ours, git_refs(repo), "listing must match for-each-ref");

    assert_eq!(
        store.read("HEAD").unwrap().unwrap(),
        RefTarget::Symbolic("refs/heads/main".into()),
        "real HEAD lives in the tables, not the compat dummy file"
    );
    assert_eq!(
        store.resolve("HEAD").unwrap().unwrap().to_string(),
        common::git(repo, &["rev-parse", "HEAD"]).trim()
    );
    store
}

/// 150 branches via one `update-ref --stdin` batch (one new table),
/// enough that compaction produces a multi-block table.
fn add_load_refs(repo: &Path, n: usize) {
    let head = common::git(repo, &["rev-parse", "HEAD"]);
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["update-ref", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for i in 0..n {
        writeln!(stdin, "update refs/heads/load/{i:04} {}", head.trim()).unwrap();
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

fn reftable_matches_git(algo: HashAlgo, object_format: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    common::make_repo_opts(repo, object_format, "reftable");

    // fresh stack (several small tables from the fixture's ref churn)
    assert_matches_git(repo, algo);

    // tombstone: deleted branch must vanish from the merged view
    common::git(repo, &["branch", "-D", "feat"]);
    let store = assert_matches_git(repo, algo);
    assert!(store.read("refs/heads/feat").unwrap().is_none());

    // annotated tag record carries the peeled target (value type 0x2)
    let tag = store
        .iter_refs()
        .unwrap()
        .into_iter()
        .find(|r| r.name == "refs/tags/v0")
        .unwrap();
    let git_peel = common::git(repo, &["rev-parse", "v0^{}"]);
    assert_eq!(tag.peeled.unwrap().to_string(), git_peel.trim());

    // many refs + compaction → multi-block single table
    add_load_refs(repo, 150);
    assert_matches_git(repo, algo);
    common::git(repo, &["pack-refs"]);
    assert_matches_git(repo, algo);
}

#[test]
fn reftable_matches_git_sha1() {
    reftable_matches_git(HashAlgo::Sha1, "sha1");
}

#[test]
fn reftable_matches_git_sha256() {
    reftable_matches_git(HashAlgo::Sha256, "sha256");
}
