//! Spin up the daemon against a tiny `.git` fixture, hit each endpoint
//! over the loopback, and check the JSON shape. End-to-end coverage that
//! the dispatcher → handler → repo path actually returns useful bytes
//! for the marketing API.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use alt_web::Source;

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn build_fixture_repo(dir: &Path) {
    run_git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "hi\n").unwrap();
    run_git(dir, &["add", "README.md"]);
    run_git(dir, &["commit", "-q", "-m", "first"]);
    std::fs::write(dir.join("README.md"), "hi again\n").unwrap();
    run_git(dir, &["add", "README.md"]);
    run_git(dir, &["commit", "-q", "-m", "second commit\n\nbody\n"]);
}

fn pick_free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

fn wait_until_listening(addr: &str, deadline: Duration) {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("alt-web did not start listening on {addr} within {deadline:?}");
}

fn http_get(addr: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    use std::io::Write;
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: x\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).unwrap();
    let (head, body) = buf.split_once("\r\n\r\n").unwrap_or((&buf, ""));
    let status_line = head.lines().next().unwrap_or("");
    let status: u16 = status_line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("bad status line: {status_line}"));
    (status, body.to_string())
}

#[test]
fn endpoints_serve_version_stats_changelog() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture_repo(dir.path());

    let port = pick_free_port();
    let addr = format!("127.0.0.1:{port}");
    let source = Source::new(dir.path().to_path_buf());
    let addr_clone = addr.clone();
    let _handle = thread::spawn(move || {
        // serve blocks until the server handle is dropped at process exit;
        // we let the test process tear it down.
        let _ = alt_web::router::serve(&addr_clone, source, 1);
    });
    wait_until_listening(&addr, Duration::from_secs(5));

    // /api/version
    let (status, body) = http_get(&addr, "/api/version");
    assert_eq!(status, 200, "version body: {body}");
    assert!(body.contains("\"schema_version\":1"), "version: {body}");
    assert!(body.contains("\"version\":\""), "version: {body}");

    // /api/stats
    let (status, body) = http_get(&addr, "/api/stats");
    assert_eq!(status, 200, "stats body: {body}");
    assert!(body.contains("\"head\":\""), "stats: {body}");
    assert!(body.contains("\"refs\":"), "stats: {body}");

    // /api/changelog?n=10 — two commits in the fixture
    let (status, body) = http_get(&addr, "/api/changelog?n=10");
    assert_eq!(status, 200, "changelog body: {body}");
    assert!(body.contains("\"commits\":["), "changelog: {body}");
    assert!(body.contains("\"second commit\""), "changelog: {body}");
    assert!(body.contains("\"first\""), "changelog: {body}");

    // /api/changelog with no ?n falls back to default
    let (status, body) = http_get(&addr, "/api/changelog");
    assert_eq!(status, 200);
    assert!(body.contains("\"commits\":["), "changelog: {body}");

    // unknown path 404
    let (status, _) = http_get(&addr, "/no-such-route");
    assert_eq!(status, 404);
}

#[test]
fn missing_alt_repo_surfaces_repo_unavailable_503() {
    let bad_dir = std::env::temp_dir().join("does-not-exist-alt-web-fixture");
    let _ = std::fs::remove_dir_all(&bad_dir);
    let port = pick_free_port();
    let addr = format!("127.0.0.1:{port}");
    let source = Source::new(bad_dir);
    let addr_clone = addr.clone();
    let _handle = thread::spawn(move || {
        let _ = alt_web::router::serve(&addr_clone, source, 1);
    });
    wait_until_listening(&addr, Duration::from_secs(5));

    // version is independent of the repo — still 200
    let (status, _) = http_get(&addr, "/api/version");
    assert_eq!(status, 200);

    // stats needs the repo — 503
    let (status, body) = http_get(&addr, "/api/stats");
    assert_eq!(status, 503, "stats body: {body}");
    assert!(body.contains("\"kind\":\"repo_unavailable\""), "{body}");
}
