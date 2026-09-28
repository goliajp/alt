//! `alt import <dir>` materialises HEAD's tree into `<dir>` so the
//! work tree is immediately clean. Previously the user had to follow up
//! with `alt switch <branch>` by hand.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn git(cwd: &Path, args: &[&str]) {
    let s = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .output()
        .unwrap();
    assert!(
        s.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&s.stderr),
    );
}

fn build_git_source(src: &Path) {
    git(src, &["init", "-q", "-b", "main"]);
    fs::write(src.join("a.txt"), "alpha\n").unwrap();
    fs::write(src.join("b.txt"), "beta\n").unwrap();
    git(src, &["add", "."]);
    git(src, &["commit", "-q", "-m", "c1"]);
}

#[test]
fn import_into_fresh_dir_materialises_head_tree() {
    let src_tmp = tempfile::tempdir().unwrap();
    let dst_tmp = tempfile::tempdir().unwrap();
    let src = src_tmp.path();
    let dst = dst_tmp.path();
    build_git_source(src);

    // alt import is discovered from `cwd`; run it from inside the
    // source repo and point at the empty dst.
    let out = ok(alt(src, &["import", &dst.display().to_string()]));
    assert!(out.contains("materialized main"), "got: {out}");

    // Work tree is checked out.
    assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "alpha\n");
    assert_eq!(fs::read_to_string(dst.join("b.txt")).unwrap(), "beta\n");

    // status is clean — previously this said "deleted: a.txt … b.txt".
    let status = ok(alt(dst, &["status"]));
    assert!(
        status.contains("nothing to commit"),
        "expected clean status, got: {status}",
    );
}

#[test]
fn import_into_existing_work_tree_does_not_clobber_files() {
    let src_tmp = tempfile::tempdir().unwrap();
    let dst_tmp = tempfile::tempdir().unwrap();
    let src = src_tmp.path();
    let dst = dst_tmp.path();
    build_git_source(src);

    // Drop a file into dst before import; the materialize step must
    // skip checkout so we don't smash the user's working copy.
    fs::write(dst.join("user.txt"), "do not clobber\n").unwrap();

    let out = ok(alt(src, &["import", &dst.display().to_string()]));
    // No materialize line — the dir wasn't empty.
    assert!(
        !out.contains("materialized "),
        "should not materialize over existing files: {out}",
    );
    assert_eq!(
        fs::read_to_string(dst.join("user.txt")).unwrap(),
        "do not clobber\n",
    );
    // The .alt store still landed though.
    assert!(dst.join(".alt").is_dir());
}

#[test]
fn a_sha256_repository_imports_and_keeps_working() {
    let src_tmp = tempfile::tempdir().unwrap();
    let dst_tmp = tempfile::tempdir().unwrap();
    let exp_tmp = tempfile::tempdir().unwrap();
    let (src, dst) = (src_tmp.path(), dst_tmp.path());
    git(src, &["init", "-q", "-b", "main", "--object-format=sha256"]);
    fs::create_dir(src.join("d")).unwrap();
    fs::write(src.join("a.txt"), "alpha\n").unwrap();
    fs::write(src.join("d/b.txt"), "beta\n").unwrap();
    git(src, &["add", "."]);
    git(src, &["commit", "-q", "-m", "c1"]);

    let out = ok(alt(src, &["import", &dst.display().to_string()]));
    assert!(out.contains("materialized main"), "got: {out}");
    assert_eq!(fs::read_to_string(dst.join("d/b.txt")).unwrap(), "beta\n");
    assert!(ok(alt(dst, &["status"])).contains("nothing to commit"));

    fs::write(dst.join("a.txt"), "changed\n").unwrap();
    ok(alt(dst, &["add", "."]));
    ok(alt(dst, &["commit", "-m", "c2"]));
    assert!(ok(alt(dst, &["status"])).contains("nothing to commit"));

    let exp = exp_tmp.path().join("r");
    ok(alt(dst, &["export", &exp.display().to_string()]));
    git(&exp, &["fsck", "--strict"]);
    git(&exp, &["checkout", "-q", "main"]);
    assert_eq!(fs::read_to_string(exp.join("a.txt")).unwrap(), "changed\n");
}
