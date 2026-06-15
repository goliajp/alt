//! `alt clone` end-to-end: init + remote add + fetch + switch under one
//! command (M6/W6). Closes 段 A of the wire milestone.

use std::path::Path;
use std::process::{Command, Output};

use crate::wire_test_server;

fn alt(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(cwd)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn ok(label: &str, o: Output) -> String {
    assert!(
        o.status.success(),
        "{label} failed: stderr={} stdout={}",
        String::from_utf8_lossy(&o.stderr),
        String::from_utf8_lossy(&o.stdout),
    );
    String::from_utf8(o.stdout).unwrap()
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Build a non-empty server repo with `main` (default HEAD) + `feat`.
fn build_server_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main", "."]);
    git(repo, &["config", "user.email", "srv@example.com"]);
    git(repo, &["config", "user.name", "Server"]);
    std::fs::write(repo.join("readme.md"), "hello\n").unwrap();
    std::fs::create_dir(repo.join("sub")).unwrap();
    std::fs::write(repo.join("sub/n.txt"), "nested\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "first"]);
    git(repo, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(repo.join("f.txt"), "feature\n").unwrap();
    git(repo, &["add", "f.txt"]);
    git(repo, &["commit", "-q", "-m", "feat"]);
    git(repo, &["checkout", "-q", "main"]);
    git(repo, &["repack", "-adq"]);
    dir
}

/// `alt clone` against a real git upload-pack: the target directory gets
/// an `.alt` store, origin is configured, every server object is fetched
/// into the local odb, `refs/heads/main` exists locally at the server's
/// tip, HEAD is symbolic to `refs/heads/main`, and the working tree is
/// materialised (the committed files appear on disk).
#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn alt_clone_round_trips_init_fetch_switch() {
    let server = build_server_repo();
    let url = wire_test_server::spawn(server.path().to_owned());

    let parent = tempfile::tempdir().unwrap();
    let target_name = "myrepo";
    ok(
        "alt clone",
        alt(parent.path(), &["clone", &url, target_name]),
    );
    let clone_dir = parent.path().join(target_name);
    assert!(clone_dir.join(".alt").is_dir(), "clone should create .alt/");

    // origin is configured with the URL we passed
    let body = std::fs::read_to_string(clone_dir.join(".alt/remotes/origin")).unwrap();
    assert!(body.contains(&format!("url={url}")), "{body}");

    // server's `main` tip mirrored into both refs/remotes and the local
    // refs/heads, plus HEAD pointed at the local main
    let server_main = git(server.path(), &["rev-parse", "refs/heads/main"])
        .trim()
        .to_owned();
    let log_out = ok(
        "alt log",
        alt(&clone_dir, &["log", "--pretty=oneline", "-n", "1"]),
    );
    let head_oid = log_out.split_whitespace().next().unwrap();
    assert_eq!(head_oid, server_main, "clone HEAD must equal server main");

    // the working tree was checked out — committed files are present
    assert!(clone_dir.join("readme.md").is_file());
    assert!(clone_dir.join("sub/n.txt").is_file());
}

/// Cloning into an existing non-empty directory is a hard error — clone
/// must not overwrite the user's work.
#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn alt_clone_refuses_to_overwrite_a_non_empty_dir() {
    let server = build_server_repo();
    let url = wire_test_server::spawn(server.path().to_owned());

    let parent = tempfile::tempdir().unwrap();
    let target = parent.path().join("there");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("user-work.txt"), "important\n").unwrap();

    let out = alt(parent.path(), &["clone", &url, "there"]);
    assert!(!out.status.success(), "clone into non-empty dir must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("not empty") || err.contains("already exists"),
        "unexpected error: {err}"
    );
    // the existing file is untouched
    assert!(target.join("user-work.txt").is_file());
}
