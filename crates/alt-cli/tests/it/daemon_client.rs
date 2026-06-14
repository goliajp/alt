//! D3: the `alt` client routes hot read commands (`status`/`branch`/`diff`)
//! through the per-repo `altd` daemon, auto-spawning one if none is up, and
//! falls back to running directly when the daemon is disabled or unreachable.
//! These drive the real `alt` binary end to end and check the two properties
//! that matter: routing through the daemon yields the *same* output as the
//! direct path, and disabling the daemon keeps the command working (local-first).

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

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
