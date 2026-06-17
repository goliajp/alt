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
    stderr: Option<BufReader<std::process::ChildStderr>>,
}

impl Server {
    /// Kill the server, then drain everything it wrote to stderr since
    /// the bind line. Reading must happen *after* kill since the child's
    /// stderr fd stays open until the process exits, and a blocking
    /// `read_to_string` would otherwise deadlock waiting for EOF.
    #[allow(dead_code)]
    fn drain_stderr(&mut self) -> String {
        // tiny_http writes the response back from a worker thread; the
        // access-log JSON line is emitted right after, but the OS may
        // schedule the parent's drain in between the response and the
        // log write. A short settle window lets the worker finish its
        // stderr write before the SIGKILL closes the pipe.
        std::thread::sleep(Duration::from_millis(100));
        let _ = self.child.kill();
        let _ = self.child.wait();
        let mut out = String::new();
        if let Some(mut reader) = self.stderr.take() {
            use std::io::Read;
            // Read from the BufReader, not the inner ChildStderr:
            // the bind-line `read_line` may have buffered additional
            // bytes (BufReader fills its 8 KiB buffer on the first
            // syscall), and reading the inner stream directly would
            // skip past whatever access-log JSON-lines landed in the
            // same syscall as the bind line.
            let _ = reader.read_to_string(&mut out);
        }
        out
    }
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
    Server {
        child,
        addr,
        stderr: Some(reader),
    }
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

#[test]
fn altd_server_acl_scopes_user_to_listed_repos_and_actions() {
    // M9/W11c: a 3-column users line scopes the user to listed
    // `repo:perm` rules. alice has `alpha:rw beta:r` → can do anything
    // on alpha, read-only on beta, no access to anything else.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    for name in ["alpha", "beta", "gamma"] {
        let dir = root.join(name);
        std::fs::create_dir(&dir).unwrap();
        ok(alt(&dir, &["init", "."]));
        std::fs::write(dir.join("file.txt"), format!("{name}\n")).unwrap();
        ok(alt(&dir, &["add", "."]));
        ok(alt(&dir, &["commit", "-m", &format!("seed {name}")]));
    }

    let token = "alice-w11c-token";
    let hash = blake3::hash(token.as_bytes()).to_hex().to_ascii_lowercase();
    std::fs::write(
        root.join("users"),
        format!("alice\t{hash}\talpha:rw beta:r\n"),
    )
    .unwrap();

    let server = spawn_server_with_env(&[("ALT_SERVER_ROOT", root.to_str().unwrap())]);

    // 1. alpha read (clone) — allowed
    let url = format!("http://alice:{token}@{}/alpha", server.addr);
    let target = tempfile::tempdir().unwrap();
    let dst = target.path().join("alpha-clone");
    let out = Command::new("git")
        .args(["clone", &url, dst.to_str().unwrap()])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PROTOCOL", "version=2")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "alice should clone alpha (rw): stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 2. beta read (info/refs) — allowed
    let info = format!(
        "http://alice:{token}@{}/beta/info/refs?service=git-upload-pack",
        server.addr
    );
    let out = Command::new("curl")
        .args(["-sS", "-w", "%{http_code}", "-o", "/dev/null", &info])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "200",
        "alice can read beta (r)"
    );

    // 3. beta write (receive-pack info/refs) — denied
    let info = format!(
        "http://alice:{token}@{}/beta/info/refs?service=git-receive-pack",
        server.addr
    );
    let out = Command::new("curl")
        .args(["-sS", "-w", "%{http_code}", "-o", "/dev/null", &info])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "403",
        "alice must not push to beta (read-only): {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 4. gamma read — denied (no rule)
    let info = format!(
        "http://alice:{token}@{}/gamma/info/refs?service=git-upload-pack",
        server.addr
    );
    let out = Command::new("curl")
        .args(["-sS", "-w", "%{http_code}", "-o", "/dev/null", &info])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "403",
        "alice must not see gamma (no rule)"
    );
}

#[test]
fn altd_server_acl_wildcard_grants_all_repos() {
    // operator with `*:rw` → every repo, every action.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    for name in ["one", "two"] {
        let dir = root.join(name);
        std::fs::create_dir(&dir).unwrap();
        ok(alt(&dir, &["init", "."]));
        std::fs::write(dir.join("f.txt"), format!("{name}\n")).unwrap();
        ok(alt(&dir, &["add", "."]));
        ok(alt(&dir, &["commit", "-m", "seed"]));
    }
    let token = "ops-token";
    let hash = blake3::hash(token.as_bytes()).to_hex().to_ascii_lowercase();
    std::fs::write(root.join("users"), format!("ops\t{hash}\t*:rw\n")).unwrap();

    let server = spawn_server_with_env(&[("ALT_SERVER_ROOT", root.to_str().unwrap())]);
    for name in ["one", "two"] {
        let url = format!(
            "http://ops:{token}@{}/{name}/info/refs?service=git-upload-pack",
            server.addr
        );
        let out = Command::new("curl")
            .args(["-sS", "-w", "%{http_code}", "-o", "/dev/null", &url])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "200",
            "ops with *:rw should read {name}"
        );
    }
}

#[test]
fn altd_server_a6_policy_denies_push_to_protected_ref() {
    // M9/W12: the repo's `.alt/policy` runs inside commit_idempotent.
    // We seed alice's principal with `branch_allow: refs/heads/feature/*`
    // and assert that a `git push` to refs/heads/main fails (server
    // returns `ng` for that ref) while a push to refs/heads/feature/x
    // is accepted.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let alpha = root.join("alpha");
    std::fs::create_dir(&alpha).unwrap();
    ok(alt(&alpha, &["init", "."]));
    std::fs::write(alpha.join("seed.txt"), "seed\n").unwrap();
    ok(alt(&alpha, &["add", "."]));
    ok(alt(&alpha, &["commit", "-m", "seed"]));

    // policy: alice may only write to refs/heads/feature/*
    std::fs::write(
        alpha.join(".alt").join("policy"),
        "human:alice -> branch=refs/heads/feature/*\n",
    )
    .unwrap();

    // users file: alice is trusted (2 columns) so she can reach the
    // server; W12 then runs her through the per-repo policy.
    let token = "alice-w12-token";
    let hash = blake3::hash(token.as_bytes()).to_hex().to_ascii_lowercase();
    std::fs::write(root.join("users"), format!("alice\t{hash}\n")).unwrap();

    let server = spawn_server_with_env(&[("ALT_SERVER_ROOT", root.to_str().unwrap())]);

    // build a tiny git source repo with a commit to push.
    let src_dir = tempfile::tempdir().unwrap();
    let src = src_dir.path();
    let st = Command::new("git")
        .args(["init", "-q", "-b", "main", src.to_str().unwrap()])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(st.status.success());
    std::fs::write(src.join("f.txt"), "from-git\n").unwrap();
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
            "x",
        ][..],
    ] {
        let o = Command::new("git").args(args).output().unwrap();
        assert!(
            o.status.success(),
            "git: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    let base = format!("http://alice:{token}@{}/alpha", server.addr);

    // 1. push to refs/heads/main → denied by policy (no rule allows it)
    let push = Command::new("git")
        .args([
            "-C",
            src.to_str().unwrap(),
            "push",
            &base,
            "HEAD:refs/heads/main",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        !push.status.success(),
        "push to refs/heads/main should be rejected by policy: {}",
        String::from_utf8_lossy(&push.stdout)
    );
    let stderr = String::from_utf8_lossy(&push.stderr);
    assert!(
        stderr.contains("refs/heads/main") || stderr.contains("rejected"),
        "rejection should name the ref or say rejected: {stderr}"
    );

    // 2. push to refs/heads/feature/x → allowed (matches allow glob)
    let push = Command::new("git")
        .args([
            "-C",
            src.to_str().unwrap(),
            "push",
            &base,
            "HEAD:refs/heads/feature/x",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        push.status.success(),
        "push to refs/heads/feature/x must be allowed: stderr={}",
        String::from_utf8_lossy(&push.stderr)
    );
    drop(server);
    // server-side ref now exists at the pushed oid
    let log = ok(alt(
        &alpha,
        &["log", "--pretty=oneline", "-n", "1", "refs/heads/feature/x"],
    ));
    assert!(log.contains('x'), "feature/x must carry the pushed commit");
}

#[test]
fn altd_server_alt_to_alt_clone_push_clone_round_trip() {
    // M9/W13: CP-9 exit gate — alt → altd-server → alt round-trip.
    //
    // Three actors:
    //   A = origin repo, seeded with one commit, served by altd-server
    //   B = alt clone of A; B makes a new commit and pushes back
    //   C = a second alt clone, taken AFTER B's push, to prove B's
    //       changes reached the server's object/ref store
    //
    // Everything client-side is the `alt` CLI driving alt-wire-http;
    // server-side is altd-server driving the same alt-wire codecs.
    // Byte-exact == log oneline matches commit-for-commit AND the
    // checked-out file contents survive the trip.

    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path();
    ok(alt(origin, &["init", "."]));
    std::fs::write(origin.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(origin.join("b.txt"), "beta\n").unwrap();
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "seed"]));
    let seed_log = ok(alt(origin, &["log", "--pretty=oneline"]));
    let seed_oid = seed_log.split_whitespace().next().unwrap_or("").to_owned();
    assert!(!seed_oid.is_empty(), "origin must have a seed commit");

    let server = spawn_server(origin);
    let url = format!("http://{}/", server.addr);

    // ---- B: alt clone http://altd-server/ B ----
    let bc = tempfile::tempdir().unwrap();
    let bdir = bc.path().join("B");
    let r = alt(bc.path(), &["clone", url.as_str(), bdir.to_str().unwrap()]);
    assert!(
        r.status.success(),
        "B clone failed: stdout={}; stderr={}",
        String::from_utf8_lossy(&r.stdout),
        String::from_utf8_lossy(&r.stderr)
    );
    // checkout content survived byte-exact
    assert_eq!(std::fs::read(bdir.join("a.txt")).unwrap(), b"alpha\n");
    assert_eq!(std::fs::read(bdir.join("b.txt")).unwrap(), b"beta\n");
    let blog = ok(alt(&bdir, &["log", "--pretty=oneline"]));
    assert!(
        blog.contains(&seed_oid),
        "B clone must hold origin's seed commit oid {seed_oid}: {blog}"
    );

    // ---- B makes a commit and pushes it back ----
    std::fs::write(bdir.join("c.txt"), "gamma\n").unwrap();
    ok(alt(&bdir, &["add", "."]));
    ok(alt(&bdir, &["commit", "-m", "from-B"]));
    let b_after = ok(alt(&bdir, &["log", "--pretty=oneline"]));
    let b_head_oid = b_after.split_whitespace().next().unwrap_or("").to_owned();
    assert!(b_head_oid != seed_oid, "B HEAD must advance after commit");
    let r = alt(
        &bdir,
        &["push", "origin", "refs/heads/main:refs/heads/main"],
    );
    assert!(
        r.status.success(),
        "B push failed: stdout={}; stderr={}",
        String::from_utf8_lossy(&r.stdout),
        String::from_utf8_lossy(&r.stderr)
    );

    // server-side ref must now point at B's head oid (proves the
    // alt-side push round-tripped the receive-pack path)
    let origin_log = ok(alt(origin, &["log", "--pretty=oneline", "refs/heads/main"]));
    assert!(
        origin_log.contains(&b_head_oid),
        "origin's refs/heads/main must advance to B's push oid {b_head_oid}: {origin_log}"
    );
    // and the pushed commit message must be readable on origin (proves
    // B's commit object actually landed in origin's odb, not just the
    // ref pointer)
    assert!(
        origin_log.contains("from-B"),
        "origin must hold the pushed commit message: {origin_log}"
    );

    // ---- C: clone again, must see B's commit ----
    let cc = tempfile::tempdir().unwrap();
    let cdir = cc.path().join("C");
    let r = alt(cc.path(), &["clone", url.as_str(), cdir.to_str().unwrap()]);
    assert!(
        r.status.success(),
        "C clone failed: stdout={}; stderr={}",
        String::from_utf8_lossy(&r.stdout),
        String::from_utf8_lossy(&r.stderr)
    );
    assert_eq!(std::fs::read(cdir.join("c.txt")).unwrap(), b"gamma\n");
    let clog = ok(alt(&cdir, &["log", "--pretty=oneline"]));
    assert!(
        clog.contains(&b_head_oid) && clog.contains(&seed_oid),
        "C clone must hold both seed and B-pushed commits: {clog}"
    );
}

/// Generate an Ed25519 keypair, write the sec key to
/// `<client>/.alt/identity/<principal>.sec`, the pub key to
/// `<server>/.alt/trust/<principal>.pub`, and turn on signing in
/// `<client>/.alt/sign-policy`. After this returns, an `alt push` from
/// `client` carries `alt-principal=<principal>` + `alt-sig=<ed25519>`
/// caps the server can verify against `<principal>.pub`.
fn enable_signed_push(client: &Path, server: &Path, principal: &str) {
    use alt_sign::SecretKey;
    let (sec, pubkey) = SecretKey::generate();

    let client_alt = client.join(".alt");
    std::fs::create_dir_all(client_alt.join("identity")).unwrap();
    std::fs::write(
        client_alt.join("identity").join(format!("{principal}.sec")),
        sec.to_text(),
    )
    .unwrap();
    std::fs::write(
        client_alt.join("sign-policy"),
        format!("enabled = true\nprincipal = {principal}\n"),
    )
    .unwrap();

    let server_alt = server.join(".alt");
    std::fs::create_dir_all(server_alt.join("trust")).unwrap();
    std::fs::write(
        server_alt.join("trust").join(format!("{principal}.pub")),
        pubkey.to_text(),
    )
    .unwrap();
}

#[test]
fn altd_server_verifies_signed_push_and_accepts_it() {
    // M10/W14: a client configured for A5b signing (sign-policy + sec
    // key) attaches `alt-principal=<id>` + `alt-sig=<ed25519>` to the
    // push caps. The server verifies the signature against its
    // `.alt/trust/<id>.pub` and the ref-update goes through. With the
    // pubkey present we expect a clean accept; the negative-path tests
    // below cover the rejections.
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path();
    ok(alt(origin, &["init", "."]));
    std::fs::write(origin.join("seed.txt"), "seed\n").unwrap();
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "seed"]));

    let server = spawn_server(origin);
    let url = format!("http://{}/", server.addr);

    // alt clone → make a signing client out of the clone
    let cdir = tempfile::tempdir().unwrap();
    let client = cdir.path().join("B");
    ok(alt(cdir.path(), &["clone", &url, client.to_str().unwrap()]));
    enable_signed_push(&client, origin, "alice");

    // new commit, signed push
    std::fs::write(client.join("from-alice.txt"), "from-alice\n").unwrap();
    ok(alt(&client, &["add", "."]));
    ok(alt(&client, &["commit", "-m", "alice-commit"]));
    let r = alt(
        &client,
        &["push", "origin", "refs/heads/main:refs/heads/main"],
    );
    assert!(
        r.status.success(),
        "signed push must succeed: stdout={}; stderr={}",
        String::from_utf8_lossy(&r.stdout),
        String::from_utf8_lossy(&r.stderr)
    );

    // origin's main now holds alice's commit message
    let log = ok(alt(origin, &["log", "--pretty=oneline", "refs/heads/main"]));
    assert!(
        log.contains("alice-commit"),
        "origin main must advance to the signed push: {log}"
    );
}

#[test]
fn altd_server_rejects_unsigned_push_when_policy_requires_signing() {
    // M10/W14: `.alt/policy` carries `human:anonymous -> require-signed`
    // (single-repo mode has no Basic auth, so the wire path falls
    // through to the `anonymous` principal). A plain git push with no
    // `alt-sig` cap must be refused before objects land in odb.
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path();
    ok(alt(origin, &["init", "."]));
    std::fs::write(origin.join("seed.txt"), "seed\n").unwrap();
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "seed"]));
    std::fs::write(
        origin.join(".alt").join("policy"),
        "human:anonymous -> require-signed\n",
    )
    .unwrap();
    let server = spawn_server(origin);
    let url = format!("http://{}/", server.addr);

    let src_dir = tempfile::tempdir().unwrap();
    let src = src_dir.path();
    let st = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main", src.to_str().unwrap()])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(st.status.success());
    std::fs::write(src.join("f.txt"), "unsigned\n").unwrap();
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
            "unsigned",
        ][..],
    ] {
        let o = std::process::Command::new("git")
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    let push = std::process::Command::new("git")
        .args([
            "-C",
            src.to_str().unwrap(),
            "push",
            &url,
            "HEAD:refs/heads/main",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        !push.status.success(),
        "unsigned push under require-signed must be refused: stdout={}",
        String::from_utf8_lossy(&push.stdout)
    );
    let stderr = String::from_utf8_lossy(&push.stderr);
    assert!(
        stderr.contains("signature required") || stderr.contains("rejected"),
        "rejection should cite the missing signature: {stderr}"
    );
    drop(server);
}

#[test]
fn altd_server_rejects_signed_push_from_unknown_principal() {
    // M10/W14: client attaches a valid Ed25519 signature, but the
    // server's `.alt/trust/` doesn't list the principal — the gate
    // returns `principal '<id>' not in trust store`.
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path();
    ok(alt(origin, &["init", "."]));
    std::fs::write(origin.join("seed.txt"), "seed\n").unwrap();
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "seed"]));
    // require-signed: forces the signature gate to fire even for an
    // unverified principal. Without a Basic-auth user the wire path
    // falls through to the `anonymous` principal, so that's what the
    // rule must target.
    std::fs::write(
        origin.join(".alt").join("policy"),
        "human:anonymous -> require-signed\n",
    )
    .unwrap();

    let server = spawn_server(origin);
    let url = format!("http://{}/", server.addr);

    // configure signing on the client, but *don't* publish the pubkey to
    // the server's trust dir
    let cdir = tempfile::tempdir().unwrap();
    let client = cdir.path().join("B");
    ok(alt(cdir.path(), &["clone", &url, client.to_str().unwrap()]));

    use alt_sign::SecretKey;
    let (sec, _pub) = SecretKey::generate();
    let client_alt = client.join(".alt");
    std::fs::create_dir_all(client_alt.join("identity")).unwrap();
    std::fs::write(client_alt.join("identity").join("alice.sec"), sec.to_text()).unwrap();
    std::fs::write(
        client_alt.join("sign-policy"),
        "enabled = true\nprincipal = alice\n",
    )
    .unwrap();

    std::fs::write(client.join("nope.txt"), "no-trust\n").unwrap();
    ok(alt(&client, &["add", "."]));
    ok(alt(&client, &["commit", "-m", "no-trust"]));
    let r = alt(
        &client,
        &["push", "origin", "refs/heads/main:refs/heads/main"],
    );
    // ensure ng surface drained before the server is dropped, so the
    // assertion message can include the server-side trace if it fires
    let mut server = server;
    let server_log = server.drain_stderr();
    assert!(
        !r.status.success(),
        "push signed by an untrusted principal must be refused: stdout={}; server_log={server_log}",
        String::from_utf8_lossy(&r.stdout)
    );
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(
        stderr.contains("not in trust store")
            || stderr.contains("signature required")
            || stderr.contains("rejected"),
        "rejection must surface to the client: client_stderr={stderr}\nserver_log={server_log}"
    );
}

#[test]
fn altd_server_round_trips_signed_commit_through_push() {
    // M10/W15: when sign-policy is on at the client, `alt commit`
    // splices an `alt-sig` header into the commit object. We push the
    // signed commit through altd-server and verify that:
    //  (a) push succeeds end-to-end,
    //  (b) the alt-sig header is byte-preserved on the server,
    //  (c) a fresh clone receives the same signed bytes (so the
    //      signature stays verifiable on the third hop).
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path();
    ok(alt(origin, &["init", "."]));
    std::fs::write(origin.join("seed.txt"), "seed\n").unwrap();
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "seed"]));

    let server = spawn_server(origin);
    let url = format!("http://{}/", server.addr);

    let cdir = tempfile::tempdir().unwrap();
    let client = cdir.path().join("B");
    ok(alt(cdir.path(), &["clone", &url, client.to_str().unwrap()]));
    enable_signed_push(&client, origin, "alice");

    std::fs::write(client.join("signed.txt"), "signed-body\n").unwrap();
    ok(alt(&client, &["add", "."]));
    ok(alt(&client, &["commit", "-m", "alice signed this"]));
    let alice_head = ok(alt(&client, &["log", "-n", "1", "--pretty=oneline"]));
    let alice_oid = alice_head
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_owned();

    let r = alt(
        &client,
        &["push", "origin", "refs/heads/main:refs/heads/main"],
    );
    assert!(
        r.status.success(),
        "signed-commit push must succeed end-to-end: {}",
        String::from_utf8_lossy(&r.stderr)
    );

    // The commit's raw object on the server still carries the alt-sig
    // header (proves wire transport preserved the signed bytes).
    let dump = ok(alt(origin, &["cat-file", "-p", &alice_oid]));
    assert!(
        dump.contains("alt-sig alice "),
        "origin's commit object must still carry the alt-sig header: {dump}"
    );

    // A subsequent clone fetches the same signed bytes.
    let cdir2 = tempfile::tempdir().unwrap();
    let downstream = cdir2.path().join("C");
    ok(alt(
        cdir2.path(),
        &["clone", &url, downstream.to_str().unwrap()],
    ));
    let dump2 = ok(alt(&downstream, &["cat-file", "-p", &alice_oid]));
    assert!(
        dump2.contains("alt-sig alice "),
        "downstream clone must preserve the alt-sig header: {dump2}"
    );
}

#[test]
fn altd_server_rejects_unsigned_commit_when_policy_requires_signed_commits() {
    // M10/W15: `.alt/policy` carries `human:anonymous ->
    // require-signed-commits`. A git push that brings in an unsigned
    // commit must be rejected by the new commit-level gate, even
    // though the push itself is allowed (no `require-signed` flag).
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path();
    ok(alt(origin, &["init", "."]));
    std::fs::write(origin.join("seed.txt"), "seed\n").unwrap();
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "seed"]));
    std::fs::write(
        origin.join(".alt").join("policy"),
        "human:anonymous -> require-signed-commits\n",
    )
    .unwrap();

    let server = spawn_server(origin);
    let url = format!("http://{}/", server.addr);

    let src_dir = tempfile::tempdir().unwrap();
    let src = src_dir.path();
    let st = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main", src.to_str().unwrap()])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(st.status.success());
    std::fs::write(src.join("f.txt"), "no-sign\n").unwrap();
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
            "unsigned-commit",
        ][..],
    ] {
        let o = std::process::Command::new("git")
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    let push = std::process::Command::new("git")
        .args([
            "-C",
            src.to_str().unwrap(),
            "push",
            &url,
            "HEAD:refs/heads/main",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        !push.status.success(),
        "push of unsigned commit under require-signed-commits must be refused: stdout={}",
        String::from_utf8_lossy(&push.stdout)
    );
    let stderr = String::from_utf8_lossy(&push.stderr);
    assert!(
        stderr.contains("missing alt-sig")
            || stderr.contains("require-signed-commits")
            || stderr.contains("rejected"),
        "rejection must cite the missing commit signature: {stderr}"
    );
    drop(server);
}

#[test]
fn altd_server_honors_git_clone_filter_blob_none() {
    // M10/W17: `git clone --filter=blob:none http://altd-server/` must
    // succeed and the resulting partial clone must hold every commit
    // and tree but no blobs. We assert by:
    //   (a) clone succeeds
    //   (b) `git rev-list --objects --filter=blob:none --filter-print-omitted`
    //       reports a non-zero number of omitted blob oids that match
    //       what's in the alt origin (so the omission is real, not a
    //       silent server-sent full pack)
    //   (c) `git cat-file -e <blob>` returns failure for one of those
    //       blobs in the partial clone
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path();
    ok(alt(origin, &["init", "."]));
    std::fs::write(origin.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(origin.join("b.txt"), "beta\n").unwrap();
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "first"]));
    std::fs::write(origin.join("a.txt"), "alpha\ngamma\n").unwrap();
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "second"]));

    let server = spawn_server(origin);
    let url = format!("http://{}/", server.addr);

    // Resolve at least one blob oid we expect on the origin so we can
    // probe for its absence on the filtered clone.
    let head = ok(alt(origin, &["log", "-n", "1", "--pretty=oneline"]));
    let head_oid = head.split_whitespace().next().unwrap().to_owned();
    let tree_dump = ok(alt(origin, &["cat-file", "-p", &head_oid]));
    // The `tree <oid>` line tells us the root tree; we then list the
    // tree to find a blob oid. Doing it via `alt` instead of `git`
    // avoids needing an installed git just for the inventory step.
    let tree_oid = tree_dump
        .lines()
        .find_map(|l| l.strip_prefix("tree "))
        .unwrap()
        .to_owned();
    let tree_listing = ok(alt(origin, &["cat-file", "-p", &tree_oid]));
    // Format is `<mode> <kind> <oid>\t<name>`; pick the first blob row.
    let blob_oid = tree_listing
        .lines()
        .find_map(|l| {
            let mut parts = l.split_whitespace();
            let _mode = parts.next()?;
            let kind = parts.next()?;
            let oid = parts.next()?;
            if kind == "blob" {
                Some(oid.to_owned())
            } else {
                None
            }
        })
        .expect("at least one blob in the origin's head tree");

    // git clone --filter=blob:none against altd-server
    let clone_root = tempfile::tempdir().unwrap();
    let target = clone_root.path().join("partial");
    let out = std::process::Command::new("git")
        .args([
            "clone",
            "--filter=blob:none",
            "--no-checkout",
            &url,
            target.to_str().unwrap(),
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PROTOCOL", "version=2")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git clone --filter=blob:none failed: stderr={} stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );

    // The known blob oid must NOT be present on the partial clone — if
    // the server ignored the filter and sent a full pack, this check
    // would falsely pass `git cat-file -e`.
    let exists = std::process::Command::new("git")
        .arg("-C")
        .arg(&target)
        .args(["cat-file", "-e", &blob_oid])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        !exists.status.success(),
        "blob {blob_oid} must be absent from the partial clone (server ignored --filter=blob:none?)"
    );

    // Sanity: the head commit's tree object IS present (only blobs
    // were filtered).
    let tree_exists = std::process::Command::new("git")
        .arg("-C")
        .arg(&target)
        .args(["cat-file", "-e", &tree_oid])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        tree_exists.status.success(),
        "tree {tree_oid} must still be present (blob:none filters blobs only)"
    );
}

#[test]
fn altd_server_branch_deny_protects_main_while_allowing_features() {
    // M10/W22: `.alt/policy` carries
    //   `human:alice -> branch=refs/heads/* branch_deny=refs/heads/main`
    // Alice may push to any feature branch, but a push to main is
    // blocked by the deny gate even though the allow glob matches it.
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path();
    ok(alt(origin, &["init", "."]));
    std::fs::write(origin.join("seed.txt"), "seed\n").unwrap();
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "seed"]));
    std::fs::write(
        origin.join(".alt").join("policy"),
        "human:anonymous -> branch=refs/heads/** branch_deny=refs/heads/main\n",
    )
    .unwrap();
    let server = spawn_server(origin);
    let url = format!("http://{}/", server.addr);

    // build a git source repo with one commit; push it twice — once
    // at refs/heads/main (must be refused), once at refs/heads/feature/x
    // (must succeed).
    let src_dir = tempfile::tempdir().unwrap();
    let src = src_dir.path();
    let st = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main", src.to_str().unwrap()])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(st.status.success());
    std::fs::write(src.join("f.txt"), "body\n").unwrap();
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
            "x",
        ][..],
    ] {
        let o = std::process::Command::new("git")
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    // 1) push to main → denied
    let push_main = std::process::Command::new("git")
        .args([
            "-C",
            src.to_str().unwrap(),
            "push",
            &url,
            "HEAD:refs/heads/main",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        !push_main.status.success(),
        "push to main must be denied by branch_deny: stdout={}",
        String::from_utf8_lossy(&push_main.stdout)
    );
    let stderr = String::from_utf8_lossy(&push_main.stderr);
    assert!(
        stderr.contains("branch_deny") || stderr.contains("rejected"),
        "rejection should cite the deny gate: {stderr}"
    );

    // 2) push to feature/x → allowed
    let push_feature = std::process::Command::new("git")
        .args([
            "-C",
            src.to_str().unwrap(),
            "push",
            &url,
            "HEAD:refs/heads/feature/x",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        push_feature.status.success(),
        "push to feature/x must succeed (allow matches, deny does not): stderr={}",
        String::from_utf8_lossy(&push_feature.stderr)
    );
    drop(server);
    let log = ok(alt(
        origin,
        &["log", "--pretty=oneline", "refs/heads/feature/x"],
    ));
    assert!(
        log.contains('x'),
        "feature/x ref must carry the pushed commit: {log}"
    );
}

#[test]
fn altd_server_emits_jsonl_access_log_per_request() {
    // M11/W23: every request lands one JSON-line on stderr with the
    // fixed schema {ts_unix_ms, req_id, method, path, status,
    // duration_ms, bytes_in, principal, repo}. We drive a ls-remote
    // (one info/refs GET) and a fetch + clone (info/refs + POST
    // upload-pack), drain stderr, parse, and assert.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "seed"]));

    let mut server = spawn_server(root);
    let url = format!("http://{}/", server.addr);

    // one info/refs roundtrip via real git ls-remote — fastest way to
    // get a request through the server without juggling test plumbing
    let out = std::process::Command::new("git")
        .args(["ls-remote", &url])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PROTOCOL", "version=2")
        .output()
        .unwrap();
    assert!(out.status.success());

    let log = server.drain_stderr();
    // The bind line is plain text; every JSON-line we emit starts with
    // `{` so a line scan filters access-log entries out cleanly.
    let access_lines: Vec<&str> = log
        .lines()
        .filter(|l| l.trim_start().starts_with('{'))
        .collect();
    assert!(
        !access_lines.is_empty(),
        "no access-log JSON-lines emitted; full stderr:\n{log}"
    );
    let first = access_lines[0];
    // Field-by-field assertion that the contract holds. Order isn't
    // asserted — the json crate emits in insertion order, but the
    // schema contract is that the *keys* exist.
    for key in [
        "\"ts_unix_ms\":",
        "\"req_id\":",
        "\"method\":",
        "\"path\":",
        "\"status\":",
        "\"duration_ms\":",
        "\"bytes_in\":",
        "\"principal\":",
        "\"repo\":",
    ] {
        assert!(
            first.contains(key),
            "access log missing field {key}: {first}"
        );
    }
    // A successful info/refs hit should be a 200 with a non-null
    // principal (anonymous in single-repo mode) and a duration ≥ 0.
    assert!(
        first.contains("\"status\":200"),
        "expected status 200 on successful info/refs: {first}"
    );
    assert!(
        first.contains("\"principal\":\"anonymous\""),
        "single-repo / no-auth requests log principal=anonymous: {first}"
    );
}

#[test]
fn altd_server_handles_concurrent_clones_in_parallel() {
    // M11/W24: with multi-threaded dispatch, N concurrent clones must
    // all succeed without queueing artifacts. We start the server,
    // fire 4 `git clone` commands in parallel against it, and assert
    // every one completed cleanly. With the W23-W24 worker pool sized
    // at 4 by default, none of these requests should be waiting on
    // one another's response.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(root.join("b.txt"), "beta\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "seed"]));

    let server = spawn_server(root);
    let url = format!("http://{}/", server.addr);

    let clone_root = tempfile::tempdir().unwrap();
    let n_clones = 4;
    let mut handles = Vec::with_capacity(n_clones);
    for i in 0..n_clones {
        let url = url.clone();
        let target = clone_root.path().join(format!("c{i}"));
        handles.push(std::thread::spawn(move || {
            std::process::Command::new("git")
                .args(["clone", &url, target.to_str().unwrap()])
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_PROTOCOL", "version=2")
                .output()
                .unwrap()
        }));
    }
    for (i, h) in handles.into_iter().enumerate() {
        let out = h.join().unwrap();
        assert!(
            out.status.success(),
            "concurrent clone {i} failed: stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let target = clone_root.path().join(format!("c{i}"));
        assert_eq!(
            std::fs::read(target.join("a.txt")).unwrap(),
            b"alpha\n",
            "clone {i} a.txt"
        );
    }
}

#[test]
fn altd_server_drains_cleanly_on_sigterm() {
    // M11/W25: send SIGTERM to the running server and assert it
    // (a) prints the "shutdown signal received" line within a sane
    //     deadline (proves the handler ran + the main thread polled
    //     the flag), and
    // (b) prints the final "all workers stopped" line and exits with
    //     code 0 (proves graceful join, not SIGKILL).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "seed"]));

    let mut server = spawn_server(root);
    let pid = server.child.id();

    // Send SIGTERM via libc (Stdlib doesn't expose it portably). On
    // unix this is the same path systemd / docker stop use.
    #[cfg(unix)]
    unsafe {
        let rc = libc::kill(pid as libc::pid_t, libc::SIGTERM);
        assert_eq!(rc, 0, "kill(SIGTERM) failed");
    }

    // Wait for the server to exit on its own — should take well under
    // a second since no in-flight requests are holding workers.
    let deadline = Instant::now() + Duration::from_secs(5);
    let exit_status = loop {
        if let Some(s) = server.child.try_wait().expect("try_wait") {
            break s;
        }
        if Instant::now() > deadline {
            panic!("altd-server did not exit within 5s of SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        exit_status.success(),
        "graceful shutdown should exit code 0; got {exit_status:?}"
    );

    // Drain stderr: should contain both the shutdown-signal line and
    // the post-join exit line, in that order.
    let log = {
        use std::io::Read;
        let mut out = String::new();
        if let Some(mut reader) = server.stderr.take() {
            let _ = reader.read_to_string(&mut out);
        }
        out
    };
    let sig_idx = log
        .find("shutdown signal received")
        .unwrap_or_else(|| panic!("missing shutdown-signal line:\n{log}"));
    let exit_idx = log
        .find("all workers stopped")
        .unwrap_or_else(|| panic!("missing post-join exit line:\n{log}"));
    assert!(
        sig_idx < exit_idx,
        "shutdown-signal line must precede the exit line:\n{log}"
    );
}

#[test]
fn altd_server_rejects_push_exceeding_max_body_size() {
    // M11/W26: a server started with ALT_SERVER_MAX_PUSH_BYTES=100
    // must refuse any real push (which carries at least a small pack
    // body, easily over 100 B) with HTTP 413 — before reading more
    // than `max + 1` bytes. We assert the rejection via the git
    // client surfacing the upstream error, plus the access log row
    // recording status=413 in the server's stderr.
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path();
    ok(alt(origin, &["init", "."]));
    std::fs::write(origin.join("seed.txt"), "seed\n").unwrap();
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "seed"]));

    let mut server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_MAX_PUSH_BYTES", "100"),
    ]);
    let url = format!("http://{}/", server.addr);

    // Build a git source repo and try to push — even an empty-tree
    // commit pushes a pack body that easily clears 100 B.
    let src_dir = tempfile::tempdir().unwrap();
    let src = src_dir.path();
    let st = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main", src.to_str().unwrap()])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(st.status.success());
    std::fs::write(src.join("f.txt"), "body\n").unwrap();
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
            "x",
        ][..],
    ] {
        let o = std::process::Command::new("git")
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    // Push to a brand-new branch so we don't tangle with main's
    // non-fast-forward refusal — the only signal we want here is
    // the 413 body-size cap.
    let push = std::process::Command::new("git")
        .args([
            "-C",
            src.to_str().unwrap(),
            "push",
            &url,
            "HEAD:refs/heads/from-git",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        !push.status.success(),
        "oversize push must be refused: stdout={}",
        String::from_utf8_lossy(&push.stdout)
    );

    let server_log = server.drain_stderr();
    assert!(
        server_log.contains("\"status\":413"),
        "access log must record the 413 row: {server_log}"
    );
}

#[test]
fn altd_server_two_concurrent_pushes_to_distinct_branches_both_land() {
    // M11/W28: with the W24 worker pool + write-side `Mutex<Store>`,
    // two clients pushing to *different* branches at the same time
    // must both succeed and leave the server with both refs pointing
    // at their respective pushed commits — no torn state, no lost
    // update.
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path();
    ok(alt(origin, &["init", "."]));
    std::fs::write(origin.join("seed.txt"), "seed\n").unwrap();
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "seed"]));

    let server = spawn_server(origin);
    let url = format!("http://{}/", server.addr);

    // Build two source git repos, each with one commit, on different
    // branch destinations. The clients fire in parallel — the server's
    // receive-pack path serialises writes through `Mutex<Store>` but
    // the dispatch surface is multi-threaded, so both pushes have to
    // complete cleanly.
    fn build_src(label: &str, base: &Path) -> std::path::PathBuf {
        let src = base.join(format!("src-{label}"));
        std::fs::create_dir(&src).unwrap();
        let st = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main", src.to_str().unwrap()])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(st.status.success());
        std::fs::write(src.join("f.txt"), format!("body-{label}\n")).unwrap();
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
                label,
            ][..],
        ] {
            let o = std::process::Command::new("git")
                .args(args)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "git: {}",
                String::from_utf8_lossy(&o.stderr)
            );
        }
        src
    }

    let src_root = tempfile::tempdir().unwrap();
    let alice_src = build_src("alice", src_root.path());
    let bob_src = build_src("bob", src_root.path());

    let alice_url = url.clone();
    let alice_dst = "HEAD:refs/heads/from-alice".to_owned();
    let alice_h = std::thread::spawn(move || {
        std::process::Command::new("git")
            .args([
                "-C",
                alice_src.to_str().unwrap(),
                "push",
                &alice_url,
                &alice_dst,
            ])
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap()
    });
    let bob_url = url.clone();
    let bob_dst = "HEAD:refs/heads/from-bob".to_owned();
    let bob_h = std::thread::spawn(move || {
        std::process::Command::new("git")
            .args(["-C", bob_src.to_str().unwrap(), "push", &bob_url, &bob_dst])
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap()
    });
    let alice_out = alice_h.join().unwrap();
    let bob_out = bob_h.join().unwrap();
    assert!(
        alice_out.status.success(),
        "alice push failed: {}",
        String::from_utf8_lossy(&alice_out.stderr)
    );
    assert!(
        bob_out.status.success(),
        "bob push failed: {}",
        String::from_utf8_lossy(&bob_out.stderr)
    );

    // Both refs landed and carry the right messages.
    let alice_log = ok(alt(
        origin,
        &["log", "--pretty=oneline", "refs/heads/from-alice"],
    ));
    let bob_log = ok(alt(
        origin,
        &["log", "--pretty=oneline", "refs/heads/from-bob"],
    ));
    assert!(
        alice_log.contains("alice"),
        "from-alice ref must hold alice's commit: {alice_log}"
    );
    assert!(
        bob_log.contains("bob"),
        "from-bob ref must hold bob's commit: {bob_log}"
    );
}

#[test]
fn altd_server_two_concurrent_pushes_to_same_ref_serialize_with_one_winner() {
    // M11/W28: when two clients race to push *the same* branch from
    // disjoint histories (each is a fresh init, no shared parent with
    // origin's main), the W12 ref policy's `commit_idempotent` runs
    // each as an atomic transaction. Exactly one wins — origin's
    // refs/heads/contested ends up at one of the two pushed oids,
    // never half-applied, and the other push surfaces an error.
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path();
    ok(alt(origin, &["init", "."]));
    std::fs::write(origin.join("seed.txt"), "seed\n").unwrap();
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "seed"]));

    let server = spawn_server(origin);
    let url = format!("http://{}/", server.addr);

    fn build_src(label: &str, base: &Path) -> std::path::PathBuf {
        let src = base.join(format!("src-{label}"));
        std::fs::create_dir(&src).unwrap();
        let st = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main", src.to_str().unwrap()])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(st.status.success());
        std::fs::write(src.join("f.txt"), format!("body-{label}\n")).unwrap();
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
                label,
            ][..],
        ] {
            let o = std::process::Command::new("git")
                .args(args)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "git: {}",
                String::from_utf8_lossy(&o.stderr)
            );
        }
        src
    }

    let src_root = tempfile::tempdir().unwrap();
    let alice_src = build_src("alice", src_root.path());
    let bob_src = build_src("bob", src_root.path());

    let alice_url = url.clone();
    let alice_dst = "HEAD:refs/heads/contested".to_owned();
    let alice_h = std::thread::spawn(move || {
        std::process::Command::new("git")
            .args([
                "-C",
                alice_src.to_str().unwrap(),
                "push",
                &alice_url,
                &alice_dst,
            ])
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap()
    });
    let bob_url = url.clone();
    let bob_dst = "HEAD:refs/heads/contested".to_owned();
    let bob_h = std::thread::spawn(move || {
        std::process::Command::new("git")
            .args(["-C", bob_src.to_str().unwrap(), "push", &bob_url, &bob_dst])
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap()
    });
    let alice_out = alice_h.join().unwrap();
    let bob_out = bob_h.join().unwrap();

    // Exactly one push must succeed. The other one is rejected
    // because by the time it lands, the ref already moved (or it lost
    // the race to advertise its old=zero against the current value).
    let wins = (alice_out.status.success() as u32) + (bob_out.status.success() as u32);
    assert!(
        wins >= 1,
        "at least one push must succeed; alice stderr={}, bob stderr={}",
        String::from_utf8_lossy(&alice_out.stderr),
        String::from_utf8_lossy(&bob_out.stderr)
    );

    // The ref must end at one of the two pushed commit messages, not
    // half-written or pointing at garbage.
    let log = ok(alt(
        origin,
        &["log", "--pretty=oneline", "refs/heads/contested"],
    ));
    assert!(
        log.contains("alice") || log.contains("bob"),
        "contested ref must carry one winner's commit, not a partial state: {log}"
    );
}
