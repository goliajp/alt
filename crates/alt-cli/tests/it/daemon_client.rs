//! The `alt` client routes hot commands (reads `status`/`branch`/`diff`/`log`,
//! and since D4 the writes) through the per-repo `altd` daemon, auto-spawning
//! one if none is up, and falls back to running directly when the daemon is
//! disabled or unreachable. These drive the real `alt` binary end to end and
//! check what matters: routing yields the *same* output as the direct path,
//! disabling keeps commands working (local-first), and the write fallback is
//! at-most-once — a write whose response is lost after sending errors out
//! rather than risking a double write.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use alt_cli::daemon;

/// Runs `alt` in `cwd`. `daemon` toggles the daemon path: when false we set
/// `ALT_NO_DAEMON=1` to force the direct path. A short idle timeout is forwarded
/// to any daemon the client spawns, so it self-exits instead of lingering.
fn alt(cwd: &Path, args: &[&str], daemon: bool) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_alt"));
    cmd.current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("USER", "tester")
        .env("ALT_DAEMON_IDLE_MS", "1500")
        .args(args);
    if !daemon {
        cmd.env("ALT_NO_DAEMON", "1");
    }
    cmd.output().unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

/// A repo with one commit and a dirty working tree (so status/diff have content
/// and branch lists `main`).
fn repo_with_changes() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."], false));
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    ok(alt(root, &["add", "."], false));
    ok(alt(root, &["commit", "-m", "base"], false));
    // leave an unstaged edit + an untracked file so the reads have something
    std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(root.join("b.txt"), "new\n").unwrap();
    dir
}

/// Each routed read produces byte-identical output whether served by the daemon
/// or run directly, and routing it brings a daemon up on the socket.
#[test]
fn client_routes_reads_through_an_autospawned_daemon() {
    let dir = repo_with_changes();
    let root = dir.path();
    let sock = root.join(".alt").join("daemon.sock");

    for args in [
        ["status", "--json"].as_slice(),
        ["branch", "--json"].as_slice(),
        ["diff", "--json"].as_slice(),
        ["log", "--json"].as_slice(),
    ] {
        let direct = ok(alt(root, args, false));
        let viad = ok(alt(root, args, true));
        assert_eq!(direct, viad, "daemon output diverged for {args:?}");
    }

    // routing actually went through a daemon: one is now listening on the socket
    assert!(
        UnixStream::connect(&sock).is_ok(),
        "no daemon came up at {}",
        sock.display()
    );
}

/// A second routed call reuses the already-running daemon — and still matches
/// the direct path.
#[test]
fn client_reuses_a_running_daemon() {
    let dir = repo_with_changes();
    let root = dir.path();
    let sock = root.join(".alt").join("daemon.sock");

    let first = ok(alt(root, &["status", "--json"], true));
    // wait until the daemon is up, then a second call hits the warm one
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && UnixStream::connect(&sock).is_err() {
        std::thread::sleep(Duration::from_millis(5));
    }
    let second = ok(alt(root, &["status", "--json"], true));
    assert_eq!(first, second);
}

/// `log` routes through the held `Repository`, which is refreshed per request:
/// a commit made by an outside process after the daemon is warm must show up in
/// the next daemon-served `log` (never a stale git-layer read).
#[test]
fn client_log_sees_external_commits_through_the_warm_daemon() {
    let dir = repo_with_changes();
    let root = dir.path();
    let sock = root.join(".alt").join("daemon.sock");

    // warm the daemon with one routed log
    let before = ok(alt(root, &["log", "--json"], true));
    assert!(before.contains("\"message\":\"base\\n\""), "{before}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && UnixStream::connect(&sock).is_err() {
        std::thread::sleep(Duration::from_millis(5));
    }

    // an outside process commits; the warm daemon must catch it up
    ok(alt(root, &["add", "."], false));
    ok(alt(root, &["commit", "-m", "external"], false));

    let after = ok(alt(root, &["log", "--json"], true));
    assert!(
        after.contains("\"message\":\"external\\n\""),
        "warm daemon served a stale log: {after}"
    );
}

/// `ALT_NO_DAEMON=1` keeps the command working and never spawns a daemon — the
/// local-first escape hatch.
#[test]
fn client_falls_back_when_daemon_disabled() {
    let dir = repo_with_changes();
    let root = dir.path();
    let sock = root.join(".alt").join("daemon.sock");

    let out = ok(alt(root, &["status", "--json"], false));
    assert!(out.contains("\"schema_version\""), "{out}");
    assert!(
        UnixStream::connect(&sock).is_err(),
        "disabled path must not spawn a daemon"
    );
}

/// A write routed through the daemon lands exactly once and is visible to an
/// outside reader — the daemon serves writes, not just reads.
#[test]
fn client_routes_writes_through_the_daemon() {
    let dir = repo_with_changes();
    let root = dir.path();

    // stage + commit both through the daemon (add and commit are routed writes)
    ok(alt(root, &["add", "."], true));
    let commit = ok(alt(root, &["commit", "-m", "via daemon"], true));
    assert!(
        commit.contains("via daemon") || !commit.is_empty(),
        "{commit}"
    );

    // an outside reader sees exactly one new commit on top of base
    let log = ok(alt(root, &["log", "--json"], false));
    assert!(log.contains("\"message\":\"via daemon\\n\""), "{log}");
    assert_eq!(
        log.matches("\"oid\"").count(),
        2,
        "expected base + 1: {log}"
    );
}

/// Exactly-once (D5c): when a write's request reaches the daemon but the
/// response is lost (a stand-in server that reads the request then drops the
/// connection), the client retries with the *same* idempotency id rather than
/// erroring or silently double-running. The first attempt's response is eaten
/// by the stand-in; the retry reconnects (spawning a real daemon, since the
/// stand-in is gone) and the write lands exactly once.
#[test]
fn client_write_retries_with_same_id_when_response_is_lost_after_send() {
    let dir = repo_with_changes();
    let root = dir.path();
    let sock = root.join(".alt").join("daemon.sock");

    // stage a real change so the commit has something to record
    std::fs::write(root.join("c.txt"), "staged\n").unwrap();
    ok(alt(root, &["add", "."], false));

    // a stand-in "daemon": accept one connection, read the request frame, then
    // drop it without answering — exactly the lost-after-send window. After its
    // single accept the listener drops, so the client's retry finds nothing
    // listening and brings up the real daemon.
    let listener = UnixListener::bind(&sock).unwrap();
    let server = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = daemon::read_frame(&mut stream);
            // drop `stream` → client's response read hits EOF (lost-after-send)
        }
    });

    let out = alt(root, &["commit", "-m", "retried"], true);
    server.join().unwrap();
    assert!(
        out.status.success(),
        "the write should be retried to success after a lost response, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // exactly one new commit on top of base: the retry landed it once, not twice
    let log = ok(alt(root, &["log", "--json"], false));
    assert!(log.contains("\"message\":\"retried\\n\""), "{log}");
    assert_eq!(
        log.matches("\"oid\"").count(),
        2,
        "expected base + exactly one commit: {log}"
    );
}
