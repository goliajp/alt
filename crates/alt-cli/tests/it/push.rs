//! `alt push` end-to-end against a real `git receive-pack` over the same
//! shared HTTP shim used by the fetch test (M6/W5).
//!
//! Verifies the full v1 path: ref advertisement parse → reachability
//! traversal → plain-pack write → POST receive-pack → report-status. The
//! server's bare repo is the integrity boundary — `git fsck --strict`
//! must accept the pushed objects, and `git rev-parse` must read back
//! exactly the oids alt's local refs hold.

use std::path::Path;
use std::process::{Command, Output};

use crate::wire_test_server;

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

/// Spin up a bare git repo to act as the push target. `--bare` so it can
/// receive pushes without checking out a working tree.
fn empty_server_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "--bare", "-b", "main", "."]);
    // a bare repo refuses receive-pack-with-no-config by default; allow
    // updating the currently-checked-out branch (bare repos have none, but
    // git still wants the option set) and don't deny non-ff for the test
    git(repo, &["config", "receive.denyCurrentBranch", "ignore"]);
    git(repo, &["config", "receive.denyNonFastForwards", "false"]);
    dir
}

/// Make a small alt repo with two commits on `main`. Returns the working
/// directory of the alt repo (also its `--root` for `alt …`).
fn build_local_alt_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok("alt init", alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/b.txt"), "nested\n").unwrap();
    ok("alt add", alt(root, &["add", "."]));
    ok("alt commit", alt(root, &["commit", "-m", "first"]));
    std::fs::write(root.join("a.txt"), "hello v2\n").unwrap();
    ok("alt add", alt(root, &["add", "a.txt"]));
    ok("alt commit (2)", alt(root, &["commit", "-m", "second"]));
    dir
}

/// End-to-end: alt push from a freshly-committed local store into an
/// empty bare git repo over the shared test shim. The server's
/// `refs/heads/main` must end up at alt's local tip, every reachable
/// object must be present (verified via `git fsck`), and the tree at
/// HEAD must contain the files we committed locally.
#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn alt_push_round_trips_into_empty_bare_repo() {
    let server_repo = empty_server_repo();
    let url = wire_test_server::spawn(server_repo.path().to_owned());

    let alt_repo = build_local_alt_repo();
    let root = alt_repo.path();
    ok(
        "alt remote add",
        alt(root, &["remote", "add", "origin", &url]),
    );

    let stdout = ok("alt push", alt(root, &["push", "origin", "--json"]));
    assert!(stdout.contains("\"remote\":\"origin\""), "{stdout}");
    assert!(stdout.contains("\"status\":\"ok\""), "{stdout}");
    assert!(stdout.contains("refs/heads/main"), "{stdout}");

    // server now has refs/heads/main pointing at alt's local main tip
    let alt_log = ok(
        "alt log",
        alt(root, &["log", "--pretty=oneline", "-n", "1"]),
    );
    let alt_main_oid = alt_log.split_whitespace().next().unwrap();

    let server_main = git(server_repo.path(), &["rev-parse", "refs/heads/main"])
        .trim()
        .to_owned();
    assert_eq!(
        server_main, alt_main_oid,
        "server's refs/heads/main must match alt's local tip"
    );

    // every object the server claims is reachable must parse cleanly
    let fsck = Command::new("git")
        .current_dir(server_repo.path())
        .args(["fsck", "--strict", "--no-dangling"])
        .output()
        .unwrap();
    assert!(
        fsck.status.success(),
        "git fsck failed: stdout={} stderr={}",
        String::from_utf8_lossy(&fsck.stdout),
        String::from_utf8_lossy(&fsck.stderr)
    );

    // and the working tree we committed (`a.txt`, `sub/b.txt`) is what
    // server-side ls-tree -r HEAD shows
    let ls = git(server_repo.path(), &["ls-tree", "-r", "HEAD"]);
    assert!(ls.contains("\ta.txt"), "ls-tree missing a.txt: {ls}");
    assert!(
        ls.contains("\tsub/b.txt"),
        "ls-tree missing sub/b.txt: {ls}"
    );
}

/// A second push against the same server is a no-op (every object is
/// already there, the ref didn't move). Exercises the
/// `everything-up-to-date` short-circuit.
#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn alt_push_is_idempotent_when_local_did_not_move() {
    let server_repo = empty_server_repo();
    let url = wire_test_server::spawn(server_repo.path().to_owned());

    let alt_repo = build_local_alt_repo();
    let root = alt_repo.path();
    ok(
        "alt remote add",
        alt(root, &["remote", "add", "origin", &url]),
    );

    ok("alt push (1)", alt(root, &["push", "origin"]));
    let stdout2 = ok("alt push (2)", alt(root, &["push", "origin"]));
    assert!(
        stdout2.contains("up to date"),
        "second push should be a no-op: {stdout2}"
    );
}

/// Pushing a second time after a new local commit ships only the
/// incremental object set (commit + tree + blob), not the whole history
/// again. The wire pack contains exactly the new objects.
#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn alt_push_ships_only_new_objects_on_incremental_update() {
    let server_repo = empty_server_repo();
    let url = wire_test_server::spawn(server_repo.path().to_owned());

    let alt_repo = build_local_alt_repo();
    let root = alt_repo.path();
    ok(
        "alt remote add",
        alt(root, &["remote", "add", "origin", &url]),
    );
    ok("alt push (1)", alt(root, &["push", "origin"]));

    // a new commit locally
    std::fs::write(root.join("c.txt"), "new\n").unwrap();
    ok("alt add", alt(root, &["add", "c.txt"]));
    ok("alt commit", alt(root, &["commit", "-m", "add c"]));

    let stdout = ok("alt push (2)", alt(root, &["push", "origin", "--json"]));
    // exactly three new objects ship: the new commit, its tree, and the
    // new blob (a.txt and sub/b.txt's blobs are already on the server,
    // so the tree's children for those entries don't need to re-ship)
    assert!(
        stdout.contains("\"objects\":3"),
        "incremental push should ship exactly 3 objects: {stdout}"
    );

    // server tip advanced to alt's new local tip
    let alt_log = ok(
        "alt log",
        alt(root, &["log", "--pretty=oneline", "-n", "1"]),
    );
    let alt_main_oid = alt_log.split_whitespace().next().unwrap();
    let server_main = git(server_repo.path(), &["rev-parse", "refs/heads/main"])
        .trim()
        .to_owned();
    assert_eq!(server_main, alt_main_oid);
}
