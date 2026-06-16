//! M9/W10a: end-to-end smoke against the `altd-server` binary.
//!
//! Build a tiny alt repo, spawn `altd-server` pointed at it, then run
//! `git ls-remote http://127.0.0.1:PORT/` and assert it walks away with
//! the refs the alt store actually holds. This proves the smart-http
//! info/refs entry round-trips through alt-wire's capability-advert +
//! ls-refs encoders without a real git server in the loop.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("USER", "tester")
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

struct Server {
    child: Child,
    addr: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `altd-server` on a kernel-chosen port and wait until it logs
/// the bind address — that's the handshake telling the test the server
/// is actually ready to accept the next request.
fn spawn_server(repo_dir: &Path) -> Server {
    let bin = env!("CARGO_BIN_EXE_altd-server");
    let mut child = Command::new(bin)
        .env("ALT_SERVER_REPO", repo_dir)
        .args(["--bind", "127.0.0.1:0"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn altd-server");
    let stderr = child.stderr.take().expect("server stderr");
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    let addr = loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if let Some(rest) = line.find("listening on ") {
            let after = &line[rest + "listening on ".len()..];
            let addr = after.split_whitespace().next().unwrap().to_owned();
            break addr;
        }
        if Instant::now() > deadline {
            panic!("altd-server never logged its listening address");
        }
    };
    Server { child, addr }
}

#[test]
fn altd_server_info_refs_serves_what_git_ls_remote_expects() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    ok(alt(root, &["add", "a.txt"]));
    ok(alt(root, &["commit", "-m", "first"]));
    ok(alt(root, &["branch", "feature-x"]));

    let server = spawn_server(root);
    let url = format!("http://{}/", server.addr);

    let out = Command::new("git")
        .args(["ls-remote", &url])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PROTOCOL", "version=2")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git ls-remote against altd-server failed: stderr={} stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    // The alt store advertises refs/heads/main + refs/heads/feature-x.
    assert!(
        stdout.contains("refs/heads/main"),
        "ls-remote missing main: {stdout}"
    );
    assert!(
        stdout.contains("refs/heads/feature-x"),
        "ls-remote missing feature-x: {stdout}"
    );
    // git also asks for HEAD by default and follows the symref. main's
    // oid is reported on the HEAD line.
    let head_oid = ok(alt(root, &["log", "-n", "1", "--pretty=oneline"]))
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    assert!(
        stdout.contains(&head_oid),
        "ls-remote should include HEAD/main commit oid {head_oid}: {stdout}"
    );
}

#[test]
fn altd_server_serves_git_clone_end_to_end() {
    // M9/W10b: with the upload-pack POST wired, a real `git clone http://…/`
    // should walk the server, pull the packfile, and reconstruct the
    // working tree byte-exact against what the alt store holds.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(root.join("b.txt"), "beta\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));
    std::fs::write(root.join("a.txt"), "alpha\ngamma\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "second"]));

    let server = spawn_server(root);
    let url = format!("http://{}/", server.addr);

    let clone_root = tempfile::tempdir().unwrap();
    let target = clone_root.path().join("clone-target");
    let out = Command::new("git")
        .args(["clone", &url, target.to_str().unwrap()])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PROTOCOL", "version=2")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git clone failed: stderr={} stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );

    // working tree files reconstructed byte-exact
    assert_eq!(
        std::fs::read(target.join("a.txt")).unwrap(),
        b"alpha\ngamma\n",
        "a.txt content after clone"
    );
    assert_eq!(
        std::fs::read(target.join("b.txt")).unwrap(),
        b"beta\n",
        "b.txt content after clone"
    );

    // git log on the clone matches alt's history: two commits both visible
    let log = Command::new("git")
        .arg("-C")
        .arg(&target)
        .args(["log", "--pretty=oneline"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(log.status.success());
    let log = String::from_utf8_lossy(&log.stdout);
    assert!(
        log.contains("first") && log.contains("second"),
        "clone history must hold both commits: {log}"
    );
}

#[test]
fn altd_server_rejects_unknown_service() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    ok(alt(root, &["add", "a.txt"]));
    ok(alt(root, &["commit", "-m", "first"]));

    let server = spawn_server(root);
    let url = format!("http://{}/info/refs?service=git-bogus", server.addr);

    let out = Command::new("curl")
        .args(["-sS", "-w", "%{http_code}", "-o", "/dev/null", &url])
        .output()
        .unwrap();
    let code = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        code, "400",
        "unknown service must surface as HTTP 400: got {code}"
    );
}
