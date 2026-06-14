//! `altd` (D2): the daemon holds one open store and serves commands over a Unix
//! socket. These drive it directly over the wire (the CLI client is D3), and
//! check the property that makes it correct: coherence with direct `alt`
//! invocations — the daemon refreshes per request, so it never serves a stale
//! read, and its own writes are visible to outside processes.

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

use alt_cli::daemon::{self, Request, Response};

fn alt(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(cwd)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
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

/// A spawned daemon for one repo. Killed on drop so a panic never leaks it; the
/// short idle timeout is a second backstop.
struct Daemon {
    child: Child,
    sock: PathBuf,
}

impl Daemon {
    // the child is reaped in `Drop` (kill + wait); clippy can't see across it
    #[allow(clippy::zombie_processes)]
    fn start(alt_dir: &Path) -> Daemon {
        let sock = alt_dir.join("daemon.sock");
        let child = Command::new(env!("CARGO_BIN_EXE_altd"))
            .arg(alt_dir)
            .env("ALT_DAEMON_IDLE_MS", "20000")
            .spawn()
            .unwrap();
        // wait for the socket to become connectable (deadline poll, not a fixed
        // sleep): the daemon binds before it accepts
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if UnixStream::connect(&sock).is_ok() {
                return Daemon { child, sock };
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("daemon socket {} never appeared", sock.display());
    }

    /// One request = one connection (the daemon serves a single command per
    /// connection). Returns the structured response.
    fn run(&self, cwd: &Path, args: &[&str]) -> Response {
        let req = Request {
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: cwd.to_path_buf(),
            env: vec![
                ("GIT_AUTHOR_NAME".into(), "tester".into()),
                ("GIT_AUTHOR_EMAIL".into(), "t@e".into()),
                ("USER".into(), "tester".into()),
            ],
        };
        let mut stream = UnixStream::connect(&self.sock).unwrap();
        daemon::write_frame(&mut stream, &req.encode()).unwrap();
        let mut buf = Vec::new();
        // the daemon writes one framed response then we read to EOF
        let len = read_one_frame(&mut stream, &mut buf);
        Response::decode(&buf[..len]).unwrap()
    }
}

/// Reads exactly one length-prefixed frame's bytes into `buf`, returning its
/// total length (header + payload) so `Response::decode` sees just the payload.
fn read_one_frame(stream: &mut UnixStream, out: &mut Vec<u8>) -> usize {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).unwrap();
    let n = u32::from_le_bytes(len) as usize;
    out.resize(n, 0);
    stream.read_exact(out).unwrap();
    n
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn out_str(r: &Response) -> String {
    String::from_utf8_lossy(&r.stdout).into_owned()
}

fn err_str(r: &Response) -> String {
    String::from_utf8_lossy(&r.stderr).into_owned()
}

/// The daemon serves native read and write commands against its held store.
#[test]
fn daemon_serves_commands_over_the_socket() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));

    let d = Daemon::start(&root.join(".alt"));

    // a read: status reflects the clean tree
    let r = d.run(root, &["status"]);
    assert_eq!(r.exit_code, 0, "stderr: {}", err_str(&r));
    assert!(
        out_str(&r).contains("working tree clean"),
        "{}",
        out_str(&r)
    );

    // a write through the daemon, then a read confirming it landed
    std::fs::write(root.join("b.txt"), "second\n").unwrap();
    let r = d.run(root, &["add", "."]);
    assert_eq!(r.exit_code, 0, "stderr: {}", err_str(&r));
    let r = d.run(root, &["commit", "-m", "via daemon"]);
    assert_eq!(r.exit_code, 0, "stderr: {}", err_str(&r));

    // the daemon's write is visible to an outside process
    let log = ok(alt(root, &["log", "--json"]));
    assert!(log.contains("\"message\":\"via daemon\\n\""), "{log}");
}

/// The crux: the held store is refreshed per request, so the daemon sees writes
/// committed by other processes since it opened — never a stale read.
#[test]
fn daemon_sees_external_writes_via_per_request_refresh() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "base\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));

    let d = Daemon::start(&root.join(".alt"));

    // baseline: only main exists
    let before = out_str(&d.run(root, &["branch", "--json"]));
    assert!(before.contains("\"main\""), "{before}");
    assert!(!before.contains("\"feat\""), "{before}");

    // an outside process creates a branch — the daemon's held refs predate it
    ok(alt(root, &["branch", "feat"]));

    // refs.refresh: the daemon now lists the externally created branch
    let after = out_str(&d.run(root, &["branch", "--json"]));
    assert!(
        after.contains("\"feat\""),
        "daemon missed external branch: {after}"
    );

    // an outside process commits, advancing HEAD with a new commit object — the
    // daemon must catch up BOTH refs (new HEAD) and odb (new commit) to resolve
    // HEAD's tree, or status would error / be stale
    std::fs::write(root.join("a.txt"), "changed\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "external advance"]));

    let r = d.run(root, &["status"]);
    assert_eq!(r.exit_code, 0, "status errored: {}", err_str(&r));
    assert!(
        out_str(&r).contains("working tree clean"),
        "stale status: {}",
        out_str(&r)
    );
}

/// Sends one request carrying a caller-chosen identity, on a fresh connection.
/// Mirrors `Daemon::run` but lets each concurrent caller pick its own author.
fn run_as(sock: &Path, cwd: &Path, args: &[&str], author: &str) -> Response {
    let req = Request {
        args: args.iter().map(|s| s.to_string()).collect(),
        cwd: cwd.to_path_buf(),
        env: vec![
            ("GIT_AUTHOR_NAME".into(), author.into()),
            ("GIT_AUTHOR_EMAIL".into(), format!("{author}@e")),
            ("USER".into(), author.into()),
        ],
    };
    let mut stream = UnixStream::connect(sock).unwrap();
    daemon::write_frame(&mut stream, &req.encode()).unwrap();
    let mut buf = Vec::new();
    let len = read_one_frame(&mut stream, &mut buf);
    Response::decode(&buf[..len]).unwrap()
}

/// D5a: the daemon serves requests concurrently (a thread per connection) over
/// one shared store behind a Mutex. Several callers commit at the same time,
/// each with its own identity into its own workspace/branch. The Mutex must
/// serialize the store work without corruption, and the per-request identity
/// must not bleed between concurrent threads.
#[test]
fn daemon_serves_concurrent_requests_without_identity_bleed() {
    let repo = tempfile::tempdir().unwrap();
    let trees = tempfile::tempdir().unwrap();
    let root = repo.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f.txt"), "base\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));

    const N: usize = 6;
    let mut work = Vec::new();
    for w in 0..N {
        let br = format!("b{w}");
        ok(alt(root, &["branch", &br]));
        let tree = trees.path().join(format!("w{w}"));
        let ws = format!("ws{w}");
        ok(alt(
            root,
            &["workspace", "add", &ws, tree.to_str().unwrap(), &br],
        ));
        work.push((br, tree));
    }

    let d = Daemon::start(&root.join(".alt"));

    // each thread drives the daemon concurrently: stage + commit in its own
    // workspace with a distinct author. All connections are live at once.
    let handles: Vec<_> = work
        .iter()
        .enumerate()
        .map(|(w, (_br, tree))| {
            let sock = d.sock.clone();
            let tree = tree.clone();
            let author = format!("author{w}");
            std::thread::spawn(move || {
                std::fs::write(tree.join("f.txt"), format!("from {author}\n")).unwrap();
                let add = run_as(&sock, &tree, &["add", "."], &author);
                assert_eq!(add.exit_code, 0, "add: {}", err_str(&add));
                let commit = run_as(&sock, &tree, &["commit", "-m", "concurrent"], &author);
                assert_eq!(commit.exit_code, 0, "commit: {}", err_str(&commit));
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    // every branch advanced by exactly one commit, authored by its own author —
    // identities did not bleed across the concurrent threads, and no commit was
    // lost or corrupted
    for (w, (br, _tree)) in work.iter().enumerate() {
        let log = ok(alt(root, &["log", "--json", br]));
        assert_eq!(
            log.matches("\"oid\"").count(),
            2,
            "b{w} should hold base + 1 commit: {log}"
        );
        let author = format!("author{w}");
        assert!(
            log.contains(&format!("{author} <{author}@e>")),
            "b{w} HEAD has wrong author (identity bleed?): {log}"
        );
    }
}

/// The daemon is just another participant on the store: it serves reads while
/// several outside processes commit concurrently (relaxed durability removes
/// the fsync spacing that would otherwise hide races). No corruption, no lost
/// commits, no stale/failed daemon read.
#[test]
fn daemon_reads_stay_coherent_under_concurrent_external_writers() {
    let repo = tempfile::tempdir().unwrap();
    let trees = tempfile::tempdir().unwrap();
    let root = repo.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f.txt"), "base\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));

    const N: usize = 4;
    const ROUNDS: usize = 8;
    let mut tree_v = Vec::new();
    for w in 0..N {
        let br = format!("b{w}");
        ok(alt(root, &["branch", &br]));
        let tree = trees.path().join(format!("w{w}"));
        ok(alt(
            root,
            &[
                "workspace",
                "add",
                &format!("w{w}"),
                tree.to_str().unwrap(),
                &br,
            ],
        ));
        tree_v.push(tree);
    }

    let d = Daemon::start(&root.join(".alt"));

    // writers commit back-to-back in their own processes; meanwhile a reader
    // thread hammers the daemon with native reads that must never error
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let sock = d.sock.clone();
        let root = root.to_path_buf();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut reads = 0u32;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                // open a fresh connection per request (daemon: one cmd/conn)
                if let Ok(mut s) = UnixStream::connect(&sock) {
                    let req = Request {
                        args: vec!["branch".into(), "--json".into()],
                        cwd: root.clone(),
                        env: vec![("USER".into(), "tester".into())],
                    };
                    if daemon::write_frame(&mut s, &req.encode()).is_ok() {
                        let mut hdr = [0u8; 4];
                        if s.read_exact(&mut hdr).is_ok() {
                            let n = u32::from_le_bytes(hdr) as usize;
                            let mut buf = vec![0u8; n];
                            s.read_exact(&mut buf).unwrap();
                            let resp = Response::decode(&buf).unwrap();
                            assert_eq!(
                                resp.exit_code,
                                0,
                                "daemon read errored mid-stress: {}",
                                String::from_utf8_lossy(&resp.stderr)
                            );
                            reads += 1;
                        }
                    }
                }
            }
            reads
        })
    };

    for r in 0..ROUNDS {
        let handles: Vec<_> = tree_v
            .iter()
            .cloned()
            .map(|tree| {
                std::thread::spawn(move || {
                    std::fs::write(tree.join("f.txt"), format!("round {r}\n")).unwrap();
                    let add = Command::new(env!("CARGO_BIN_EXE_alt"))
                        .current_dir(&tree)
                        .env("ALT_NO_DAEMON", "1")
                        .env("GIT_AUTHOR_NAME", "tester")
                        .env("GIT_AUTHOR_EMAIL", "t@e")
                        .env("ALT_RELAXED_DURABILITY", "1")
                        .args(["add", "."])
                        .status()
                        .unwrap();
                    assert!(add.success(), "add failed");
                    let commit = Command::new(env!("CARGO_BIN_EXE_alt"))
                        .current_dir(&tree)
                        .env("ALT_NO_DAEMON", "1")
                        .env("GIT_AUTHOR_NAME", "tester")
                        .env("GIT_AUTHOR_EMAIL", "t@e")
                        .env("ALT_RELAXED_DURABILITY", "1")
                        .args(["commit", "-m", "work"])
                        .status()
                        .unwrap();
                    assert!(commit.success(), "commit failed");
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let reads = reader.join().unwrap();
    assert!(reads > 0, "the reader never completed a request");

    // every branch holds base + ROUNDS commits and the store reads back cleanly
    for w in 0..N {
        let log = ok(alt(root, &["log", "--json", &format!("b{w}")]));
        let commits = log.matches("\"oid\"").count();
        assert_eq!(commits, ROUNDS + 1, "b{w} lost commits: {log}");
    }
    // and the daemon still serves a final coherent read of all branches
    let r = d.run(root, &["branch", "--json"]);
    assert_eq!(r.exit_code, 0, "stderr: {}", err_str(&r));
    for w in 0..N {
        assert!(
            out_str(&r).contains(&format!("\"b{w}\"")),
            "{}",
            out_str(&r)
        );
    }
}

/// D5b: under in-process group commit (concurrent commits coalesce their
/// fsyncs), no commit is lost or corrupted. Several workspaces commit many
/// rounds concurrently through the daemon; every branch must end with exactly
/// base + ROUNDS commits, and the whole store must read back cleanly afterward.
#[test]
fn daemon_group_commit_loses_no_concurrent_writes() {
    let repo = tempfile::tempdir().unwrap();
    let trees = tempfile::tempdir().unwrap();
    let root = repo.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f.txt"), "base\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));

    const N: usize = 5;
    const ROUNDS: usize = 12;
    let mut trees_v = Vec::new();
    for w in 0..N {
        let br = format!("b{w}");
        ok(alt(root, &["branch", &br]));
        let tree = trees.path().join(format!("w{w}"));
        ok(alt(
            root,
            &[
                "workspace",
                "add",
                &format!("ws{w}"),
                tree.to_str().unwrap(),
                &br,
            ],
        ));
        trees_v.push(tree);
    }

    let d = Daemon::start(&root.join(".alt"));
    let handles: Vec<_> = trees_v
        .into_iter()
        .enumerate()
        .map(|(w, tree)| {
            let sock = d.sock.clone();
            std::thread::spawn(move || {
                for r in 0..ROUNDS {
                    std::fs::write(tree.join("f.txt"), format!("w{w} r{r}\n")).unwrap();
                    let add = run_as(&sock, &tree, &["add", "."], "tester");
                    assert_eq!(add.exit_code, 0, "add: {}", err_str(&add));
                    let c = run_as(&sock, &tree, &["commit", "-m", "x"], "tester");
                    assert_eq!(c.exit_code, 0, "commit: {}", err_str(&c));
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    // every branch holds exactly base + ROUNDS commits — nothing lost to the
    // coalesced fsync — and an external process reads the store back cleanly
    for w in 0..N {
        let log = ok(alt(root, &["log", "--json", &format!("b{w}")]));
        assert_eq!(
            log.matches("\"oid\"").count(),
            ROUNDS + 1,
            "b{w} lost commits under group commit: {log}"
        );
    }
}

/// D5b bench (manual): concurrent-commit throughput through the daemon. Commits
/// are fsync-bound; the daemon's in-process group commit coalesces the fsyncs of
/// commits that overlap in flight, so aggregate throughput at higher concurrency
/// should exceed the serial (one-fsync-per-commit) rate. Prints commits/sec at a
/// few concurrency levels; not an assertion (fsync timing is hardware-dependent,
/// and the fast tier forbids ratio assertions). Run:
///   cargo test --release -p alt-cli --test it -- --ignored --nocapture daemon_commit_throughput
#[test]
#[ignore = "bench: concurrent commit throughput through the daemon, run manually"]
fn daemon_commit_throughput() {
    // each concurrency level commits into its own pool of workspaces so writers
    // never contend on the same branch (that would serialize on a ref conflict,
    // not on fsync — we want to measure the fsync coalescing)
    fn measure(concurrency: usize, rounds: usize) -> f64 {
        let repo = tempfile::tempdir().unwrap();
        let trees = tempfile::tempdir().unwrap();
        let root = repo.path();
        ok(alt(root, &["init", "."]));
        std::fs::write(root.join("f.txt"), "base\n").unwrap();
        ok(alt(root, &["add", "."]));
        ok(alt(root, &["commit", "-m", "base"]));

        let mut trees_v = Vec::new();
        for w in 0..concurrency {
            let br = format!("c{w}");
            ok(alt(root, &["branch", &br]));
            let tree = trees.path().join(format!("w{w}"));
            ok(alt(
                root,
                &[
                    "workspace",
                    "add",
                    &format!("ws{w}"),
                    tree.to_str().unwrap(),
                    &br,
                ],
            ));
            trees_v.push(tree);
        }

        let d = Daemon::start(&root.join(".alt"));
        let start = Instant::now();
        let handles: Vec<_> = trees_v
            .into_iter()
            .enumerate()
            .map(|(w, tree)| {
                let sock = d.sock.clone();
                std::thread::spawn(move || {
                    for r in 0..rounds {
                        std::fs::write(tree.join("f.txt"), format!("w{w} r{r}\n")).unwrap();
                        let add = run_as(&sock, &tree, &["add", "."], "bench");
                        assert_eq!(add.exit_code, 0, "add: {}", err_str(&add));
                        let c = run_as(&sock, &tree, &["commit", "-m", "x"], "bench");
                        assert_eq!(c.exit_code, 0, "commit: {}", err_str(&c));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let elapsed = start.elapsed().as_secs_f64();
        (concurrency * rounds) as f64 / elapsed
    }

    const ROUNDS: usize = 40;
    for concurrency in [1usize, 2, 4, 8] {
        let rate = measure(concurrency, ROUNDS);
        println!("daemon commit throughput: concurrency={concurrency:>2}  {rate:>8.1} commits/s");
    }
}
