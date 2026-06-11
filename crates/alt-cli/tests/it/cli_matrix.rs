//! The byte-exactness matrix: every alt command output is diffed against
//! git's, hermetically (no system/global git config).

use std::path::Path;
use std::process::{Command, Output};

use alt_testutil as common;

fn run(bin: &str, repo: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap()
}

/// Runs the same args through alt and git; both must succeed with
/// identical stdout bytes.
fn assert_same(repo: &Path, args: &[&str]) {
    let alt = run(env!("CARGO_BIN_EXE_alt"), repo, args);
    let git = run("git", repo, args);
    assert!(
        git.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&git.stderr)
    );
    assert!(
        alt.status.success(),
        "alt {args:?}: {}",
        String::from_utf8_lossy(&alt.stderr)
    );
    assert_eq!(
        alt.stdout.as_bstr(),
        git.stdout.as_bstr(),
        "stdout mismatch for {args:?} in {repo:?}"
    );
}

use bstr::ByteSlice;

fn matrix(repo: &Path) {
    let head = common::git(repo, &["rev-parse", "HEAD"]);
    let tree = common::git(repo, &["rev-parse", "HEAD^{tree}"]);
    let blob = common::git(repo, &["rev-parse", "HEAD:a.txt"]);
    let (head, tree, blob) = (head.trim(), tree.trim(), blob.trim());

    for rev in ["HEAD", "main", "feat", "v0", "refs/heads/main", head] {
        assert_same(repo, &["rev-parse", rev]);
    }
    for oid in [head, tree, blob] {
        for flag in ["-t", "-s", "-p"] {
            assert_same(repo, &["cat-file", flag, oid]);
        }
    }
    // annotated tag: type/size/payload without peeling
    for flag in ["-t", "-s", "-p"] {
        assert_same(repo, &["cat-file", flag, "v0"]);
    }
    for extra in [
        &["--pretty=raw"][..],
        &["--pretty=raw", "-n", "3"],
        &["--pretty=raw", "-n", "1"],
        &["--pretty=oneline"],
        &["--pretty=oneline", "-n", "2"],
        &["--pretty=raw", "feat"],
        &["--pretty=raw", "v0"],
    ] {
        let mut args = vec!["log"];
        args.extend_from_slice(extra);
        assert_same(repo, &args);
    }
}

#[test]
fn matrix_matches_git() {
    for (object_format, ref_format) in
        [("sha1", "files"), ("sha256", "files"), ("sha1", "reftable")]
    {
        let tmp = tempfile::tempdir().unwrap();
        common::make_repo_opts(tmp.path(), object_format, ref_format);
        matrix(tmp.path());
        common::pack_repo(tmp.path());
        matrix(tmp.path());
    }
}
