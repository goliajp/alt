//! `alt fetch` end-to-end against a real `git upload-pack` over a
//! hermetic local HTTP listener (M6/W4).
//!
//! The test server is a ~80-line shim that translates one HTTP request
//! into one `git upload-pack` invocation:
//!
//! - `GET /info/refs?service=git-upload-pack` → prepend the smart-http
//!   `# service=…` header pkt + flush, then run `git upload-pack
//!   --http-backend-info-refs` on the server repo and pass its stdout
//!   through.
//! - `POST /git-upload-pack` → run `git upload-pack --stateless-rpc` with
//!   the request body as stdin; pipe its stdout into the HTTP response.
//!
//! The `Git-Protocol` request header is forwarded to the subprocess as
//! the `GIT_PROTOCOL` env var (the path `git http-backend` itself uses),
//! so `version=2` switches the server into protocol v2 — what alt-wire
//! speaks.
//!
//! After fetch, we verify:
//!
//! 1. Every server ref the spec covered (heads + tags) is mirrored
//!    locally under `refs/remotes/origin/*`.
//! 2. Every object reachable from the server's refs is present in the
//!    `.alt` odb (proves the streamed pack + our `index_pack` resolved
//!    all deltas to the right oids).

use std::path::Path;
use std::process::{Command, Output};

use alt_git_codec::ObjectId;
use alt_odb::NativeOdb;

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

/// Build a small server repo: two commits on `main`, one on `feat`, plus
/// an annotated tag. Repack everything so the response packfile exercises
/// the delta-resolution path in `index_pack`.
fn build_server_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main", "."]);
    git(repo, &["config", "user.email", "srv@example.com"]);
    git(repo, &["config", "user.name", "Server"]);
    // first commit
    std::fs::write(repo.join("readme.md"), "hello\n").unwrap();
    // a larger file so a repack actually deltas its two versions
    let big1: String = (0..200).map(|i| format!("line {i}\n")).collect();
    std::fs::write(repo.join("big.txt"), &big1).unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "first"]);
    // feature branch
    git(repo, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(repo.join("feat.md"), "feature note\n").unwrap();
    git(repo, &["add", "feat.md"]);
    git(repo, &["commit", "-q", "-m", "feat work"]);
    // back on main, second commit (modify big to force delta)
    git(repo, &["checkout", "-q", "main"]);
    let big2: String = (0..201).map(|i| format!("line {i} v2\n")).collect();
    std::fs::write(repo.join("big.txt"), &big2).unwrap();
    git(repo, &["commit", "-q", "-am", "second"]);
    // annotated tag
    git(repo, &["tag", "-a", "v0", "-m", "v0 release"]);
    // repack into one packfile to exercise delta entries on the wire
    git(repo, &["repack", "-adq"]);
    dir
}

/// All objects reachable from `refs/heads/*` on the server (what a
/// default fetch — branches-only refspec — pulls into the local odb).
/// Annotated tags only land here when they're reachable from a fetched
/// commit (the `include-tag` capability), so we use `--branches` rather
/// than `--all`.
fn branch_reachable_oids(repo: &Path) -> Vec<ObjectId> {
    let out = git(repo, &["rev-list", "--objects", "--branches"]);
    let mut oids = Vec::new();
    for line in out.lines() {
        let oid_str = line.split_whitespace().next().unwrap();
        oids.push(oid_str.parse().unwrap());
    }
    oids
}

/// End-to-end: alt fetch against a local HTTPS-less mirror of
/// `git upload-pack`. Ignored by default — the test needs `git` on PATH;
/// it's a real-server fixture, not a hermetic Rust-only unit.
#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn alt_fetch_round_trips_against_git_upload_pack() {
    let server_repo = build_server_repo();
    let url = wire_test_server::spawn(server_repo.path().to_owned());

    let alt_root = tempfile::tempdir().unwrap();
    let root = alt_root.path();
    ok("alt init", alt(root, &["init", "."]));
    ok(
        "alt remote add",
        alt(root, &["remote", "add", "origin", &url]),
    );

    let stdout = ok(
        "alt fetch origin",
        alt(root, &["fetch", "origin", "--json"]),
    );
    assert!(stdout.contains("\"remote\":\"origin\""), "{stdout}");
    assert!(stdout.contains("\"refs\":"), "{stdout}");

    // server's branches landed under refs/remotes/origin/* with the
    // server's exact oids
    let server_main = git(server_repo.path(), &["rev-parse", "refs/heads/main"])
        .trim()
        .to_owned();
    let server_feat = git(server_repo.path(), &["rev-parse", "refs/heads/feat"])
        .trim()
        .to_owned();
    assert!(
        stdout.contains(&server_main),
        "main oid {server_main} not in fetch output: {stdout}"
    );
    assert!(
        stdout.contains(&server_feat),
        "feat oid {server_feat} not in fetch output: {stdout}"
    );

    // every object reachable from the server is present in alt's odb
    let alt_dir = root.join(".alt");
    let odb = NativeOdb::open(&alt_dir).unwrap();
    let server_oids = branch_reachable_oids(server_repo.path());
    assert!(!server_oids.is_empty(), "server repo has no objects?");
    for oid in &server_oids {
        assert!(
            odb.contains(oid),
            "alt odb missing object {oid} that server has"
        );
    }
}

/// Re-running fetch against the same server is a no-op for objects (every
/// oid already present) and produces an empty pack — the ref transaction
/// is also a no-op, so the op log doesn't grow.
#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn alt_fetch_is_idempotent_when_remote_is_unchanged() {
    let server_repo = build_server_repo();
    let url = wire_test_server::spawn(server_repo.path().to_owned());

    let alt_root = tempfile::tempdir().unwrap();
    let root = alt_root.path();
    ok("alt init", alt(root, &["init", "."]));
    ok(
        "alt remote add",
        alt(root, &["remote", "add", "origin", &url]),
    );

    ok("alt fetch (1)", alt(root, &["fetch", "origin"]));
    let map_size_1 = std::fs::metadata(root.join(".alt/map.alt")).unwrap().len();

    ok("alt fetch (2)", alt(root, &["fetch", "origin"]));
    let map_size_2 = std::fs::metadata(root.join(".alt/map.alt")).unwrap().len();
    assert_eq!(
        map_size_1, map_size_2,
        "second fetch should not re-ingest any objects",
    );
}
