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
/// is actually ready to accept the next request. Single-repo mode.
fn spawn_server(repo_dir: &Path) -> Server {
    spawn_server_with_env(&[("ALT_SERVER_REPO", repo_dir.to_str().unwrap())])
}

fn spawn_server_with_env(env: &[(&str, &str)]) -> Server {
    let bin = env!("CARGO_BIN_EXE_altd-server");
    let mut cmd = Command::new(bin);
    cmd.args(["--bind", "127.0.0.1:0"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn altd-server");
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
fn altd_server_accepts_git_push_end_to_end() {
    // M9/W10c: a `git push http://altd-server/ HEAD:refs/heads/from-git`
    // round-trips fully — server ingests the pack into its odb,
    // commits the ref update as one tx, and the alt-side ref appears
    // afterwards. We build the source as a git repo (since `git push`
    // is what we're testing) and serve an alt-backed receiver.
    let receiver_root_dir = tempfile::tempdir().unwrap();
    let receiver_root = receiver_root_dir.path();
    ok(alt(receiver_root, &["init", "."]));
    std::fs::write(receiver_root.join("seed.txt"), "seed\n").unwrap();
    ok(alt(receiver_root, &["add", "seed.txt"]));
    ok(alt(receiver_root, &["commit", "-m", "seed"]));

    let server = spawn_server(receiver_root);
    let url = format!("http://{}/", server.addr);

    // build a git source repo with the commit we'll push
    let src_dir = tempfile::tempdir().unwrap();
    let src = src_dir.path();
    let git_init = Command::new("git")
        .args(["init", "-q", "-b", "main", src.to_str().unwrap()])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(git_init.status.success(), "git init: {git_init:?}");
    std::fs::write(src.join("pushed.txt"), "from-git\n").unwrap();
    for args in [
        &["-C", src.to_str().unwrap(), "add", "."][..],
        &[
            "-C",
            src.to_str().unwrap(),
            "-c",
            "user.name=tester",
            "-c",
            "user.email=t@e",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "from-git",
        ][..],
    ] {
        let o = Command::new("git")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let head_oid = String::from_utf8_lossy(
        &Command::new("git")
            .args(["-C", src.to_str().unwrap(), "rev-parse", "HEAD"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_owned();

    let push = Command::new("git")
        .args([
            "-C",
            src.to_str().unwrap(),
            "push",
            &url,
            "HEAD:refs/heads/from-git",
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        push.status.success(),
        "git push failed: stderr={} stdout={}",
        String::from_utf8_lossy(&push.stderr),
        String::from_utf8_lossy(&push.stdout)
    );

    // Drop the server so its locks release before we open the same store
    // from `alt log` below. The Drop impl on Server kills + waits.
    drop(server);

    // The pushed ref shows up in the alt store: `alt log` from refs/heads/from-git
    // sees the from-git commit. Use rev-parse to assert exact oid match.
    let alt_oid = ok(alt(
        receiver_root,
        &["log", "--pretty=oneline", "-n", "1", "refs/heads/from-git"],
    ))
    .split_whitespace()
    .next()
    .unwrap()
    .to_owned();
    assert_eq!(
        alt_oid, head_oid,
        "alt-side ref must resolve to the pushed commit"
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

#[test]
fn altd_server_multi_repo_routes_url_to_named_repo() {
    // M9/W11a: under ALT_SERVER_ROOT=<dir>, URL `/<name>/...` picks
    // <dir>/<name> as the alt repo. Two side-by-side repos should
    // serve distinct histories without contamination.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let alpha = root.join("alpha");
    let beta = root.join("beta");
    std::fs::create_dir(&alpha).unwrap();
    std::fs::create_dir(&beta).unwrap();

    // alpha: "A" content
    ok(alt(&alpha, &["init", "."]));
    std::fs::write(alpha.join("a.txt"), "alpha-only\n").unwrap();
    ok(alt(&alpha, &["add", "."]));
    ok(alt(&alpha, &["commit", "-m", "alpha-commit"]));

    // beta: "B" content (distinct repo, distinct ref/oid)
    ok(alt(&beta, &["init", "."]));
    std::fs::write(beta.join("b.txt"), "beta-only\n").unwrap();
    ok(alt(&beta, &["add", "."]));
    ok(alt(&beta, &["commit", "-m", "beta-commit"]));

    let server = spawn_server_with_env(&[("ALT_SERVER_ROOT", root.to_str().unwrap())]);

    for (name, expected_file, expected_content, expected_msg) in [
        ("alpha", "a.txt", "alpha-only\n", "alpha-commit"),
        ("beta", "b.txt", "beta-only\n", "beta-commit"),
    ] {
        let url = format!("http://{}/{name}", server.addr);
        let clone_root = tempfile::tempdir().unwrap();
        let target = clone_root.path().join(format!("clone-{name}"));
        let out = Command::new("git")
            .args(["clone", &url, target.to_str().unwrap()])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_PROTOCOL", "version=2")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git clone {url}: stderr={} stdout={}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
        assert_eq!(
            std::fs::read(target.join(expected_file)).unwrap(),
            expected_content.as_bytes(),
            "cloned content from /{name}"
        );
        // and a non-target file should NOT exist (no cross-repo bleed)
        let other = if expected_file == "a.txt" {
            "b.txt"
        } else {
            "a.txt"
        };
        assert!(
            !target.join(other).exists(),
            "/{name} clone leaked {other} from the other repo"
        );
        let log = Command::new("git")
            .arg("-C")
            .arg(&target)
            .args(["log", "--pretty=oneline"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(log.status.success());
        let log_s = String::from_utf8_lossy(&log.stdout);
        assert!(
            log_s.contains(expected_msg),
            "/{name} clone log missing its own commit: {log_s}"
        );
    }
}

#[test]
fn altd_server_multi_repo_returns_404_for_unknown_name() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let alpha = root.join("alpha");
    std::fs::create_dir(&alpha).unwrap();
    ok(alt(&alpha, &["init", "."]));
    std::fs::write(alpha.join("a.txt"), "alpha\n").unwrap();
    ok(alt(&alpha, &["add", "."]));
    ok(alt(&alpha, &["commit", "-m", "seed"]));

    let server = spawn_server_with_env(&[("ALT_SERVER_ROOT", root.to_str().unwrap())]);
    let url = format!(
        "http://{}/nope/info/refs?service=git-upload-pack",
        server.addr
    );
    let out = Command::new("curl")
        .args(["-sS", "-w", "%{http_code}", "-o", "/dev/null", &url])
        .output()
        .unwrap();
    let code = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        code, "404",
        "unknown repo must surface as HTTP 404: got {code}"
    );
}

#[test]
fn altd_server_basic_auth_blocks_unauthenticated_and_allows_correct_token() {
    // M9/W11b: with a `users` file under ALT_SERVER_ROOT, every request
    // must carry Basic auth. Without it → 401; with the correct user +
    // token → the request flows as in single-repo mode.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let alpha = root.join("alpha");
    std::fs::create_dir(&alpha).unwrap();
    ok(alt(&alpha, &["init", "."]));
    std::fs::write(alpha.join("a.txt"), "alpha\n").unwrap();
    ok(alt(&alpha, &["add", "."]));
    ok(alt(&alpha, &["commit", "-m", "seed"]));

    // users file: alice carries BLAKE3("alice-token-123")
    let token = "alice-token-123";
    let hash = blake3::hash(token.as_bytes()).to_hex().to_ascii_lowercase();
    std::fs::write(root.join("users"), format!("alice\t{hash}\n")).unwrap();

    let server = spawn_server_with_env(&[("ALT_SERVER_ROOT", root.to_str().unwrap())]);
    let url = format!(
        "http://{}/alpha/info/refs?service=git-upload-pack",
        server.addr
    );

    // 1. no auth → 401 + WWW-Authenticate
    let out = Command::new("curl")
        .args([
            "-sS",
            "-w",
            "%{http_code}\n%header{www-authenticate}",
            "-o",
            "/dev/null",
            &url,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    let code = lines.next().unwrap_or("");
    let www_auth = lines.next().unwrap_or("");
    assert_eq!(code, "401", "unauthenticated must surface as 401: {stdout}");
    assert!(
        www_auth.to_lowercase().contains("basic"),
        "401 missing Basic WWW-Authenticate prompt: {www_auth}"
    );

    // 2. wrong token → 401
    let bad_url = format!(
        "http://alice:wrong@{}/alpha/info/refs?service=git-upload-pack",
        server.addr
    );
    let out = Command::new("curl")
        .args(["-sS", "-w", "%{http_code}", "-o", "/dev/null", &bad_url])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "401",
        "wrong token must 401"
    );

    // 3. correct token → 200 (server produces a capability advert body)
    let good_url = format!(
        "http://alice:{token}@{}/alpha/info/refs?service=git-upload-pack",
        server.addr
    );
    let out = Command::new("curl")
        .args(["-sS", "-w", "%{http_code}", "-o", "/dev/null", &good_url])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "200",
        "correct token must reach the handler"
    );

    // 4. a real `git clone` with credentials embedded should walk
    // through the same gate and produce the full working tree.
    let target_root = tempfile::tempdir().unwrap();
    let target = target_root.path().join("with-auth");
    let clone_url = format!("http://alice:{token}@{}/alpha", server.addr);
    let out = Command::new("git")
        .args(["clone", &clone_url, target.to_str().unwrap()])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PROTOCOL", "version=2")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git clone with auth failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(target.join("a.txt")).unwrap(),
        b"alpha\n",
        "clone behind auth still serves the right content"
    );
}
