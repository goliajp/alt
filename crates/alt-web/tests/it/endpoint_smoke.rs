//! Spin up the daemon against a tiny multi-repo root (one fixture `.alt`
//! imported from a fresh `.git`), hit each endpoint over the loopback,
//! and check the JSON shape.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use alt_repo::Repository;
use alt_web::MultiRepo;

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

fn build_fixture_alt(root: &Path, name: &str) -> PathBuf {
    let repo_dir = root.join(name);
    std::fs::create_dir_all(&repo_dir).unwrap();
    // We need a git source, then alt-import that into <repo_dir>/.alt.
    let git_src = root.join(format!(".{name}-src"));
    std::fs::create_dir_all(&git_src).unwrap();
    run_git(&git_src, &["init", "-q", "-b", "main"]);
    std::fs::write(git_src.join("README.md"), "hi\n").unwrap();
    run_git(&git_src, &["add", "README.md"]);
    run_git(&git_src, &["commit", "-q", "-m", "first"]);
    std::fs::write(git_src.join("README.md"), "hi again\n").unwrap();
    run_git(&git_src, &["add", "README.md"]);
    run_git(&git_src, &["commit", "-q", "-m", "second commit\n\nbody\n"]);

    let repo = Repository::discover(&git_src).unwrap();
    alt_import::import_git(&repo, &repo_dir.join(".alt"), "test/web-endpoints", 1).unwrap();
    repo_dir
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
fn endpoints_serve_version_repos_refs_log() {
    let tmp = tempfile::tempdir().unwrap();
    build_fixture_alt(tmp.path(), "alt");
    build_fixture_alt(tmp.path(), "playground");

    let port = pick_free_port();
    let addr = format!("127.0.0.1:{port}");
    let mr = MultiRepo::new(tmp.path().to_path_buf());
    let addr_clone = addr.clone();
    let _handle = thread::spawn(move || {
        let _ = alt_web::router::serve(&addr_clone, mr, 1);
    });
    wait_until_listening(&addr, Duration::from_secs(5));

    // /api/version
    let (status, body) = http_get(&addr, "/api/version");
    assert_eq!(status, 200);
    assert!(body.contains("\"schema_version\":1"), "{body}");

    // /api/repos
    let (status, body) = http_get(&addr, "/api/repos");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"name\":\"alt\""), "{body}");
    assert!(body.contains("\"name\":\"playground\""), "{body}");
    assert!(body.contains("\"head\":\""), "{body}");

    // /api/repos/alt
    let (status, body) = http_get(&addr, "/api/repos/alt");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"repo\":{"), "{body}");
    assert!(body.contains("\"head_branch\":\"main\""), "{body}");

    // /api/repos/alt/refs
    let (status, body) = http_get(&addr, "/api/repos/alt/refs");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"refs/heads/main\""), "{body}");

    // /api/repos/alt/log
    let (status, body) = http_get(&addr, "/api/repos/alt/log");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"second commit\""), "{body}");
    assert!(body.contains("\"first\""), "{body}");

    // /api/repos/alt/log?n=1
    let (status, body) = http_get(&addr, "/api/repos/alt/log?n=1");
    assert_eq!(status, 200);
    assert!(body.contains("\"second commit\""), "{body}");
    assert!(!body.contains("\"first\""), "{body}");

    // unknown repo
    let (status, _) = http_get(&addr, "/api/repos/nope");
    assert_eq!(status, 404);

    // unknown path
    let (status, _) = http_get(&addr, "/no-such-route");
    assert_eq!(status, 404);

    // /api/repos/alt/commits/{HEAD}/diff — fetch HEAD oid from log
    let (_, log_body) = http_get(&addr, "/api/repos/alt/log?n=1");
    let head_oid = extract_str(&log_body, "\"oid\":\"");
    let (status, body) = http_get(&addr, &format!("/api/repos/alt/commits/{head_oid}"));
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"tree\":\""), "commit detail: {body}");
    assert!(body.contains("\"parents\":["), "commit detail: {body}");
    assert!(body.contains("\"committer\":{"), "commit detail: {body}");

    let (status, body) = http_get(&addr, &format!("/api/repos/alt/commits/{head_oid}/diff"));
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"path\":\"README.md\""), "diff: {body}");
    assert!(body.contains("--- a/README.md"), "diff: {body}");

    // /api/repos/alt/tree/main — list root tree of HEAD branch
    let (status, body) = http_get(&addr, "/api/repos/alt/tree/main");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"name\":\"README.md\""), "tree: {body}");

    // /api/repos/alt/blob/{oid} — pick README.md blob oid from tree
    let tree_oid = extract_str(&body, "\"oid\":\"");
    let blob_oid = extract_str(&body, "\"name\":\"README.md\",\"oid\":\"");
    assert!(!blob_oid.is_empty(), "blob oid: {body}");
    let _ = tree_oid;
    let (status, body) = http_get(&addr, &format!("/api/repos/alt/blob/{blob_oid}"));
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"binary\":false"), "{body}");
    assert!(body.contains("hi again"), "{body}");
}

fn extract_str(s: &str, before: &str) -> String {
    let p = s
        .find(before)
        .unwrap_or_else(|| panic!("missing {before} in {s}"));
    let after = &s[p + before.len()..];
    let q = after
        .find('"')
        .unwrap_or_else(|| panic!("unterminated string after {before}"));
    after[..q].to_string()
}
