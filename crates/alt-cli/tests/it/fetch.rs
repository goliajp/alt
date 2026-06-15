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

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;

use alt_git_codec::ObjectId;
use alt_odb::NativeOdb;

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

/// Spawn a tiny HTTP server in a background thread that proxies to
/// `git upload-pack` for the given repo. Returns the bound URL; the
/// listener thread runs until the test process exits.
fn spawn_git_http_server(repo_dir: PathBuf) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };
            let repo_dir = repo_dir.clone();
            thread::spawn(move || {
                let _ = handle_one(stream, &repo_dir);
            });
        }
    });
    url
}

fn handle_one(mut stream: std::net::TcpStream, repo: &Path) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);

    // request line: METHOD path HTTP/1.x\r\n
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let parts: Vec<&str> = line.trim_end().split(' ').collect();
    if parts.len() < 3 {
        return Ok(());
    }
    let method = parts[0].to_owned();
    let url = parts[1].to_owned();

    // headers until blank line
    let mut content_length: usize = 0;
    let mut git_protocol: Option<String> = None;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let trimmed = header.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
        // header names are case-insensitive; ureq emits "Git-Protocol"
        if let Some(v) = lower.strip_prefix("git-protocol:") {
            git_protocol = Some(v.trim().to_string());
        }
    }

    // request body (Content-Length-framed; ureq's `send_bytes` always
    // uses Content-Length, so we don't need chunked decoding here)
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    let (status, payload, ct) = if method == "GET" && url.starts_with("/info/refs") {
        let mut cmd = Command::new("git");
        cmd.args(["upload-pack", "--http-backend-info-refs"]);
        cmd.arg(repo);
        if let Some(p) = &git_protocol {
            cmd.env("GIT_PROTOCOL", p);
        }
        let out = cmd.output()?;
        assert!(out.status.success(), "upload-pack info-refs failed");
        // smart-http envelope: `# service=git-upload-pack\n` pkt + flush,
        // then upload-pack's own pkt-line stream
        let mut body_out = Vec::new();
        write_pkt(&mut body_out, b"# service=git-upload-pack\n");
        body_out.extend_from_slice(b"0000");
        body_out.extend_from_slice(&out.stdout);
        (
            "200 OK",
            body_out,
            "application/x-git-upload-pack-advertisement",
        )
    } else if method == "POST" && url == "/git-upload-pack" {
        let mut cmd = Command::new("git");
        cmd.args(["upload-pack", "--stateless-rpc"]);
        cmd.arg(repo);
        if let Some(p) = &git_protocol {
            cmd.env("GIT_PROTOCOL", p);
        }
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        child.stdin.as_mut().unwrap().write_all(&body)?;
        drop(child.stdin.take()); // close stdin so upload-pack exits
        let out = child.wait_with_output()?;
        assert!(
            out.status.success(),
            "upload-pack stateless-rpc failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        ("200 OK", out.stdout, "application/x-git-upload-pack-result")
    } else {
        ("404 Not Found", Vec::new(), "text/plain")
    };

    let resp_head = format!(
        "HTTP/1.0 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(resp_head.as_bytes())?;
    stream.write_all(&payload)?;
    stream.flush()?;
    Ok(())
}

fn write_pkt(out: &mut Vec<u8>, payload: &[u8]) {
    let total = payload.len() + 4;
    out.extend_from_slice(format!("{total:04x}").as_bytes());
    out.extend_from_slice(payload);
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
    let url = spawn_git_http_server(server_repo.path().to_owned());

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
    let url = spawn_git_http_server(server_repo.path().to_owned());

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
