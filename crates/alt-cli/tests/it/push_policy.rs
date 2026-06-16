//! M6/W8 — local A6 policy gate on `alt push` (cross-party pre-check).
//!
//! Each test wires a local A6 policy and a real bare git server, then
//! verifies the push side rejects (without going to the wire) when the
//! policy says no, and accepts otherwise.

use std::path::Path;
use std::process::{Command, Output};

use crate::wire_test_server;

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("ALT_PRINCIPAL_ID", "alice")
        .env("ALT_PRINCIPAL_KIND", "agent")
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

fn empty_server_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "--bare", "-b", "main", "."]);
    git(repo, &["config", "receive.denyCurrentBranch", "ignore"]);
    git(repo, &["config", "receive.denyNonFastForwards", "false"]);
    dir
}

fn build_local_alt_with_one_commit() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok("alt init", alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    ok("alt add", alt(root, &["add", "."]));
    ok("alt commit", alt(root, &["commit", "-m", "first"]));
    dir
}

/// `read-only` capability blocks a push at the local gate before any
/// HTTP request goes out. The bare server stays empty.
#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn push_rejected_locally_when_principal_is_read_only() {
    let server = empty_server_repo();
    let url = wire_test_server::spawn(server.path().to_owned());

    let alt_repo = build_local_alt_with_one_commit();
    let root = alt_repo.path();
    ok(
        "alt remote add",
        alt(root, &["remote", "add", "origin", &url]),
    );
    // pin alice to read-only
    std::fs::write(root.join(".alt/policy"), "agent:alice -> read-only\n").unwrap();

    let out = alt(root, &["push", "origin"]);
    assert!(!out.status.success(), "read-only push must fail locally");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("read-only") || err.contains("cannot push"),
        "stderr: {err}"
    );
    // bare server got nothing
    let server_refs = git(server.path(), &["for-each-ref"]);
    assert!(server_refs.is_empty(), "server should still be empty");
}

/// `branch=feature/*` restricts push targets — pushing main is refused.
#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn push_rejected_when_branch_allow_excludes_target() {
    let server = empty_server_repo();
    let url = wire_test_server::spawn(server.path().to_owned());

    let alt_repo = build_local_alt_with_one_commit();
    let root = alt_repo.path();
    ok(
        "alt remote add",
        alt(root, &["remote", "add", "origin", &url]),
    );
    std::fs::write(
        root.join(".alt/policy"),
        "agent:alice -> branch=feature/*\n",
    )
    .unwrap();

    let out = alt(root, &["push", "origin"]);
    assert!(!out.status.success(), "non-matching branch must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("branch_allow"), "stderr: {err}");
}

/// Non-fast-forward push without `-f` is refused (git-default behaviour),
/// even without an A6 forbid_force capability. With `-f`, the same push
/// is allowed.
#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn push_non_fast_forward_requires_force_flag() {
    // server pre-seeded with a different history at refs/heads/main so
    // our local tip is NOT an ancestor
    let server = tempfile::tempdir().unwrap();
    let server_path = server.path();
    git(server_path, &["init", "-q", "--bare", "-b", "main", "."]);
    git(
        server_path,
        &["config", "receive.denyCurrentBranch", "ignore"],
    );
    git(
        server_path,
        &["config", "receive.denyNonFastForwards", "false"],
    );

    // seed the server: clone, commit, push back so refs/heads/main has
    // history we don't share
    let seed = tempfile::tempdir().unwrap();
    git(seed.path(), &["init", "-q", "-b", "main", "."]);
    git(seed.path(), &["config", "user.email", "s@e"]);
    git(seed.path(), &["config", "user.name", "seed"]);
    std::fs::write(seed.path().join("other.txt"), "seed\n").unwrap();
    git(seed.path(), &["add", "."]);
    git(seed.path(), &["commit", "-q", "-m", "server seed"]);
    git(
        seed.path(),
        &["push", server_path.to_str().unwrap(), "main"],
    );

    let url = wire_test_server::spawn(server_path.to_owned());
    let alt_repo = build_local_alt_with_one_commit();
    let root = alt_repo.path();
    ok(
        "alt remote add",
        alt(root, &["remote", "add", "origin", &url]),
    );

    // first try without -f: should refuse with a "fetch first" or
    // "non-fast-forward" message (we never saw the server's tip)
    let out = alt(root, &["push", "origin"]);
    assert!(!out.status.success(), "non-ff without -f must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("non-fast-forward")
            || err.contains("alt fetch")
            || err.contains("not in the local odb"),
        "expected non-ff/fetch error, got: {err}"
    );

    // with -f, push goes through and overwrites the remote ref
    ok("alt push -f", alt(root, &["push", "-f", "origin"]));
    let server_main = git(server_path, &["rev-parse", "refs/heads/main"])
        .trim()
        .to_owned();
    let alt_log = ok(
        "alt log",
        alt(root, &["log", "--pretty=oneline", "-n", "1"]),
    );
    let alt_main = alt_log.split_whitespace().next().unwrap();
    assert_eq!(
        server_main, alt_main,
        "force push should advance to local tip"
    );
}

/// `forbid-force` capability blocks a non-ff push even when `-f` was
/// passed — A6 is the deeper gate.
#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn forbid_force_cap_blocks_push_even_with_force_flag() {
    // same shape as above: server pre-seeded with foreign history
    let server = tempfile::tempdir().unwrap();
    let server_path = server.path();
    git(server_path, &["init", "-q", "--bare", "-b", "main", "."]);
    git(
        server_path,
        &["config", "receive.denyCurrentBranch", "ignore"],
    );
    git(
        server_path,
        &["config", "receive.denyNonFastForwards", "false"],
    );
    let seed = tempfile::tempdir().unwrap();
    git(seed.path(), &["init", "-q", "-b", "main", "."]);
    git(seed.path(), &["config", "user.email", "s@e"]);
    git(seed.path(), &["config", "user.name", "seed"]);
    std::fs::write(seed.path().join("other.txt"), "seed\n").unwrap();
    git(seed.path(), &["add", "."]);
    git(seed.path(), &["commit", "-q", "-m", "server seed"]);
    git(
        seed.path(),
        &["push", server_path.to_str().unwrap(), "main"],
    );

    let url = wire_test_server::spawn(server_path.to_owned());
    let alt_repo = build_local_alt_with_one_commit();
    let root = alt_repo.path();
    ok(
        "alt remote add",
        alt(root, &["remote", "add", "origin", &url]),
    );
    std::fs::write(root.join(".alt/policy"), "agent:alice -> forbid-force\n").unwrap();
    // fetch the remote tip first so the local odb has it (so the gate
    // sees a real non-ff, not a "fetch first" error)
    ok("alt fetch", alt(root, &["fetch", "origin"]));

    let out = alt(root, &["push", "-f", "origin"]);
    assert!(
        !out.status.success(),
        "forbid-force must override -f, got stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("forbid_force") || err.contains("not be a fast-forward"),
        "{err}"
    );
}
