//! alt's wire-protocol server (`altd-server`).
//!
//! Serves git protocol v2 over HTTP for a single alt repo (`ALT_SERVER_REPO`) or a
//! directory of repos (`ALT_SERVER_ROOT`). The contract is the smart-http v2 entry
//! every git client recognises:
//!
//!   GET  /info/refs?service=git-upload-pack    → capability advert + ls-refs
//!   GET  /info/refs?service=git-receive-pack   → capability advert + ls-refs
//!   POST /git-upload-pack                      → command-dispatch (ls-refs, fetch)
//!   POST /git-receive-pack                     → command-dispatch
//!
//! The `info/refs` handler works end-to-end: server reads the
//! repo's refs through `alt-repo::Repository`, encodes a capability
//! advertisement (advertising only ls-refs for now), and follows it
//! with the ls-refs response so a plain `git ls-remote http://…/` works.
//!
//! Usage:
//!
//!   ALT_SERVER_REPO=<path-to-repo> altd-server [--bind 127.0.0.1:PORT]
//!
//! The repo path is the directory holding `.alt` (the same form `alt
//! status` would find from inside) or a bare `.git`-shaped repo for
//! the export path. Bare = TLS lives in front of this; the design picks
//! reverse-proxy-style TLS termination so the server itself can stay
//! ureq-style plain-HTTP and minimal.

#[path = "altd_server/auth.rs"]
mod auth;
#[path = "altd_server/fetch.rs"]
mod fetch;
#[path = "altd_server/http/mod.rs"]
mod http;
#[path = "altd_server/limits.rs"]
mod limits;
#[path = "altd_server/readers.rs"]
mod readers;

use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use alt_cli::held::Held;
use alt_cli::native::{Principal, PrincipalKind, Store};
use alt_cli::receive;
use alt_git_codec::{HashAlgo, ObjectId};
use alt_repo::Repository;
use alt_wire::caps;
use alt_wire::ls_refs::RefRecord;
use alt_wire::push::CommandStatus;
use auth::{AclRule, AuthOutcome, acl_allows, action_from_request, check_auth};
use http::{Header, Method, Response, Server, StatusCode};
use limits::{max_body_bytes, max_command_bytes, pack_limits, read_body_capped};
use readers::RepoHandle;

const AGENT: &str = concat!("alt-server/", env!("CARGO_PKG_VERSION"));
const UPLOAD_PACK_RESULT: &str = "application/x-git-upload-pack-result";
const STOPPED: &str = "writes are stopped until the server is started again: a sync failed";

/// per-request log context filled in as dispatch progresses.
/// The outer serve loop reads it back after dispatch returns and emits
/// one JSON-line access log entry, so a single request is one line of
/// machine-readable observability — same style as `alt`'s `--json` paths.
#[derive(Default)]
struct LogCtx {
    status: u16,
    bytes_in: u64,
    /// response body byte count when the encoder knows the
    /// length up front (the common path — `Response::from_data`,
    /// `from_string`, and the pack/sideband encoders all hand over a
    /// finished `Vec<u8>`). `None` is rendered as JSON `null` in the
    /// access log.
    bytes_out: Option<u64>,
    /// total fsync count from the process-wide
    /// `WriteCoordinator.group.fsync_count()` snapshot at response
    /// time. Set only on receive-pack responses (the path that
    /// actually contends for durability). When N concurrent pushes
    /// share a single group fsync, their access-log entries carry
    /// the same `fsync_seq` value — the coalescing signal.
    fsync_seq: Option<u64>,
    principal: Option<String>,
    repo: Option<String>,
}

/// Monotonic short request id. A 12-char hex from an `AtomicU64` counter
/// keyed by the server's start time — enough to disambiguate within a
/// process lifetime without pulling in a uuid dep.
fn next_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{n:012x}")
}

fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Emit one JSON-line access log entry to stderr (the same channel
/// the bind line uses, so an operator's stderr capture sees both).
fn emit_access_log(req_id: &str, method: &Method, url: &str, log: &LogCtx, duration_ms: u128) {
    use alt_cli::json::Json;
    let row = Json::Object(vec![
        ("ts_unix_ms", Json::Num(unix_ms_now() as i64)),
        ("req_id", Json::str(req_id)),
        ("method", Json::str(format!("{method:?}"))),
        ("path", Json::str(url)),
        ("status", Json::Num(i64::from(log.status))),
        ("duration_ms", Json::Num(duration_ms as i64)),
        ("bytes_in", Json::Num(log.bytes_in as i64)),
        (
            "bytes_out",
            match log.bytes_out {
                Some(n) => Json::Num(n as i64),
                None => Json::Null,
            },
        ),
        (
            "fsync_seq",
            match log.fsync_seq {
                Some(n) => Json::Num(n as i64),
                None => Json::Null,
            },
        ),
        (
            "principal",
            match &log.principal {
                Some(p) => Json::str(p.clone()),
                None => Json::Null,
            },
        ),
        (
            "repo",
            match &log.repo {
                Some(r) => Json::str(r.clone()),
                None => Json::Null,
            },
        ),
    ]);
    let mut buf = Vec::with_capacity(256);
    let _ = row.write(&mut buf);
    buf.push(b'\n');
    use std::io::Write;
    let _ = std::io::stderr().write_all(&buf);
}

/// Helper that captures the response's status code into the LogCtx and
/// then hands the request off to `respond`. Replaces every bare
/// `req.respond(resp)?` site so access logs always see the real status.
fn respond_logged<R: std::io::Read>(
    req: http::Request,
    resp: Response<R>,
    log: &mut LogCtx,
) -> std::io::Result<()> {
    log.status = resp.status_code().0;
    log.bytes_out = resp.data_length().map(|n| n as u64);
    req.respond(resp)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut bind = "127.0.0.1:0".to_owned();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                i += 1;
                bind = args
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| die("--bind needs an address"));
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: altd-server [--bind 127.0.0.1:PORT]\n\n\
                     env:\n  \
                     ALT_SERVER_REPO          single alt repo to serve (legacy mode)\n  \
                     ALT_SERVER_ROOT          multi-repo root; URLs map /<name>/… → <root>/<name>\n  \
                     ALT_SERVER_WORKERS       parallel request handler threads (default 4)\n  \
                     ALT_SERVER_REQUIRE_AUTH  set to 1 to refuse start when <root>/users is absent (fail-open auth guard)"
                );
                return;
            }
            other => die(&format!("unknown arg {other:?}")),
        }
        i += 1;
    }

    let mode = match (
        std::env::var("ALT_SERVER_REPO").ok(),
        std::env::var("ALT_SERVER_ROOT").ok(),
    ) {
        (Some(_), Some(_)) => die("set either ALT_SERVER_REPO or ALT_SERVER_ROOT, not both"),
        (None, None) => {
            die("either ALT_SERVER_REPO (single repo) or ALT_SERVER_ROOT (multi-repo) must be set")
        }
        (Some(p), None) => ServeMode::single(PathBuf::from(p)),
        (None, Some(p)) => ServeMode::multi(PathBuf::from(p)),
    };

    // N workers each call `server.recv()` in a loop, so that concurrent
    // clients do not queue behind a slow push
    let workers: usize = std::env::var("ALT_SERVER_WORKERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n: &usize| *n > 0)
        .unwrap_or(4);

    // install signal handlers so SIGINT (Ctrl-C) and SIGTERM
    // (`systemctl stop`, `docker stop`, k8s preStop) tip the shutdown
    // flag. The handler must stay async-signal-safe, so it only does an
    // atomic store; the main thread polls it and drives the actual
    // shutdown sequence. Installed before the listening line: a
    // supervisor may signal as soon as it sees it, and a signal landing
    // earlier would take the default action and skip the drain.
    install_signal_handlers();

    let server =
        Arc::new(Server::http(&bind).unwrap_or_else(|e| die(&format!("bind {bind}: {e}"))));
    eprintln!(
        "altd-server: listening on {} ({}, workers={workers})",
        server.server_addr(),
        mode.describe()
    );

    let mode = Arc::new(mode);
    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let server = Arc::clone(&server);
        let mode = Arc::clone(&mode);
        handles.push(std::thread::spawn(move || {
            loop {
                let req = match server.recv() {
                    Ok(r) => r,
                    Err(_) => break, // unblocked or socket dead
                };
                serve_one(&mode, req);
            }
        }));
    }

    // Poll the shutdown flag on the main thread. Once SIGTERM/SIGINT
    // fires, unblock all worker `recv()` calls (pending recv → Err,
    // in-flight responses keep going) and join. The poll is cheap
    // (100 ms) and only the main thread runs it.
    loop {
        if SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!("altd-server: shutdown signal received, unblocking workers");
            // `unblock()` frees one blocked recv() per call
            for _ in 0..workers {
                server.unblock();
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // a stuck worker (long ingest_pack, a malicious slow
    // client, a deadlocked Mutex<Store>) must not pin the process
    // forever. Spawn a watchdog that hard-exits after the deadline if
    // `handles.join()` hasn't returned by then. systemd / k8s see
    // exit 0 either way — graceful was the goal, the deadline is the
    // hard limit, both paths are an acceptable shutdown.
    let deadline_ms: u64 = std::env::var("ALT_SERVER_SHUTDOWN_DEADLINE_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n: &u64| *n > 0)
        .unwrap_or(30_000);
    let _watchdog = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(deadline_ms));
        eprintln!("altd-server: graceful shutdown timed out after {deadline_ms} ms; force-exiting");
        std::process::exit(0);
    });

    for h in handles {
        let _ = h.join();
    }
    eprintln!("altd-server: all workers stopped, exiting cleanly");
}

/// Set when SIGINT or SIGTERM arrives. Read by the main thread to
/// drive `Server::unblock()` and graceful worker drain.
static SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn shutdown_handler(_sig: alt_os::sys::c_int) {
    // Async-signal-safe: relaxed atomic store is the only thing we do
    // inside the handler. The main thread does the actual work.
    SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(unix)]
fn install_signal_handlers() {
    let handler = shutdown_handler as *const () as alt_os::sys::sighandler_t;
    unsafe {
        alt_os::sys::signal(alt_os::sys::SIGINT, handler);
        alt_os::sys::signal(alt_os::sys::SIGTERM, handler);
    }
}

#[cfg(not(unix))]
fn install_signal_handlers() {
    // Non-unix targets: no handler. The shutdown loop still runs but
    // `SHUTDOWN` is never set externally, so the process exits when
    // an operator hard-kills it.
}

/// One request through dispatch + access-log emission. Pulled out of
/// the main loop so each worker thread runs the same path with no
/// shared mutable state across workers (everything per-request is
/// stack-local).
fn serve_one(mode: &ServeMode, req: http::Request) {
    let url = req.url().to_owned();
    let method = req.method().clone();
    let req_id = next_request_id();
    let start = std::time::Instant::now();
    let mut log = LogCtx {
        bytes_in: req.body_length().map(|n| n as u64).unwrap_or(0),
        ..LogCtx::default()
    };
    if let Err(e) = dispatch(mode, method.clone(), &url, req, &mut log) {
        eprintln!("altd-server: handler error: {e} (req_id={req_id})");
        if log.status == 0 {
            log.status = 500;
        }
    }
    let duration_ms = start.elapsed().as_millis();
    emit_access_log(&req_id, &method, &url, &log, duration_ms);
}

/// Single-repo (ALT_SERVER_REPO) or multi-repo (ALT_SERVER_ROOT) serve.
/// Single keeps the original shape — info/refs lives at the URL root.
/// Multi parses the first path segment as a repo name and resolves it
/// under the configured root.
enum ServeMode {
    Single {
        repo_path: PathBuf,
        writer: Option<Arc<WriteCoordinator>>,
        readers: readers::Readers,
    },
    Multi {
        root: PathBuf,
        cache: Held<HashMap<String, RepoHandle>>,
    },
}

/// three-piece write coordinator for one repo. `store` is what a push
/// has in hand from its look at the refs to its ref transaction; a
/// request that panics with it in hand ends the process (see
/// [`alt_cli::held`]).
/// `sink` is an independent fsync handle (own fds) that the leader
/// committer calls outside the store lock — overlap is what makes
/// group commit pay off. `group` hands out tickets + makes followers
/// wait for the leader's flush, so N concurrent receive-pack pushes
/// share ~1 fsync instead of paying N fsyncs each. Shared across
/// requests via `Arc`.
struct WriteCoordinator {
    store: Held<Store>,
    sink: alt_cli::native::StoreSink,
    group: alt_cli::group_commit::GroupCommit,
    /// single-use nonces issued during receive-pack info/refs
    /// and consumed during the matching `git-receive-pack` POST. Bounds
    /// the replay window to "between info/refs advert and EOL of the
    /// LRU table". A captured signed push payload can only be replayed
    /// against this server if its nonce hasn't yet been consumed and
    /// hasn't aged out of the table.
    nonces: NonceTable,
}

/// In-memory single-use nonce table. Cap at 1024 active entries; on
/// overflow the oldest is dropped (LRU FIFO). This is a tradeoff:
/// under burst load an honest client whose info/refs nonce ages out
/// before they POST receive-pack gets refused, but the bound makes
/// the table O(1) memory regardless of traffic.
struct NonceTable {
    inner: Held<NonceInner>,
}

struct NonceInner {
    queue: std::collections::VecDeque<String>,
    set: std::collections::HashSet<String>,
}

impl NonceTable {
    fn new() -> Self {
        NonceTable {
            inner: Held::new(NonceInner {
                queue: std::collections::VecDeque::with_capacity(1024),
                set: std::collections::HashSet::with_capacity(1024),
            }),
        }
    }

    /// Issue a fresh 32-char-hex nonce (128 bits — collision negligible
    /// against any realistic traffic + 1024-entry table). Inserts it
    /// into the table; on overflow the oldest entry is evicted.
    fn issue(&self) -> String {
        let nonce = {
            // 16 bytes from the OS entropy pool via /dev/urandom; we
            // avoid a `rand` dep by reading directly. Failure to read
            // is essentially "running on Mars" — fall back to a
            // timestamp-derived value so the path doesn't crash.
            let mut buf = [0u8; 16];
            match std::fs::File::open("/dev/urandom") {
                Ok(mut f) => {
                    use std::io::Read;
                    let _ = f.read_exact(&mut buf);
                }
                Err(_) => {
                    let ms = unix_ms_now().to_le_bytes();
                    buf[..ms.len()].copy_from_slice(&ms);
                }
            }
            let mut hex = String::with_capacity(32);
            for b in buf {
                hex.push_str(&format!("{b:02x}"));
            }
            hex
        };
        let mut g = self.inner.take();
        if g.queue.len() >= 1024
            && let Some(old) = g.queue.pop_front()
        {
            g.set.remove(&old);
        }
        g.queue.push_back(nonce.clone());
        g.set.insert(nonce.clone());
        nonce
    }

    /// Atomically check + consume a nonce. Returns true iff it was in
    /// the table at the time of call; subsequent calls return false.
    fn consume(&self, nonce: &str) -> bool {
        let mut g = self.inner.take();
        if g.set.remove(nonce) {
            // also drop it from the queue so the LRU bookkeeping stays
            // accurate; we accept the O(N) here because consumes are
            // rare relative to issues.
            if let Some(pos) = g.queue.iter().position(|s| s == nonce) {
                g.queue.remove(pos);
            }
            true
        } else {
            false
        }
    }
}

/// Open the write store and build the write coordinator: turn on deferred
/// durability so each commit appends-without-fsync, create the off-write
/// `sink` so the fsync uses independent fds, instantiate the
/// `GroupCommit` that coalesces concurrent flushes.
fn open_write_coordinator(
    alt_dir: std::path::PathBuf,
) -> Result<Arc<WriteCoordinator>, Box<dyn std::error::Error>> {
    let mut store = Store::open(alt_dir)?;
    store.set_defer_durability(true);
    let sink = store.sink()?;
    Ok(Arc::new(WriteCoordinator {
        store: Held::new(store),
        sink,
        group: alt_cli::group_commit::GroupCommit::new(),
        nonces: NonceTable::new(),
    }))
}

impl ServeMode {
    fn single(repo_path: PathBuf) -> Self {
        // Fail-fast: probe the repo once at boot so a misconfigured
        // ALT_SERVER_REPO surfaces immediately rather than on the first
        // request. We drop the handle; per-request opens are cheap.
        Repository::discover(&repo_path).unwrap_or_else(|e| die(&format!("opening repo: {e}")));
        let alt_dir = repo_path.join(".alt");
        let writer: Option<Arc<WriteCoordinator>> = if alt_dir.is_dir() {
            Some(
                open_write_coordinator(alt_dir)
                    .unwrap_or_else(|e| die(&format!("opening write coordinator: {e}"))),
            )
        } else {
            eprintln!(
                "altd-server: no .alt under {p}; receive-pack will be refused (read-only mode)",
                p = repo_path.display()
            );
            None
        };
        let readers = readers::none();
        ServeMode::Single {
            repo_path,
            writer,
            readers,
        }
    }

    fn multi(root: PathBuf) -> Self {
        if !root.is_dir() {
            die(&format!(
                "ALT_SERVER_ROOT={} does not exist or isn't a directory",
                root.display()
            ));
        }
        // fail-open auth guard. When `ALT_SERVER_REQUIRE_AUTH=1`
        // an absent or unreadable `users` file is a hard error at
        // startup — the operator who set the env explicitly opted into
        // "no auth = no serve". Without the env we keep the previous
        // permissive behaviour (no users file = no auth check), which
        // is the right default for `--bind 127.0.0.1` dev loops.
        if std::env::var("ALT_SERVER_REQUIRE_AUTH").as_deref() == Ok("1") {
            let users_path = root.join("users");
            if !users_path.is_file() {
                die(&format!(
                    "ALT_SERVER_REQUIRE_AUTH=1 but {} is missing or unreadable; \
                     refusing to start (fail-open auth disabled)",
                    users_path.display()
                ));
            }
        }
        ServeMode::Multi {
            root,
            cache: Held::new(HashMap::new()),
        }
    }

    fn describe(&self) -> String {
        match self {
            ServeMode::Single { repo_path, .. } => format!("single repo {}", repo_path.display()),
            ServeMode::Multi { root, .. } => format!("multi-repo under {}", root.display()),
        }
    }
}

/// Resolve the repo the request is targeting + return the remainder of
/// the URL path (so the dispatcher can match on `/info/refs` etc.) plus
/// the repo *name* under multi-repo mode. Single-repo mode has no
/// extractable name; we substitute the synthetic `*` so an ACL rule
/// matching every repo still applies, though scoped ACLs only fire in
/// multi mode anyway.
fn resolve_repo(
    mode: &ServeMode,
    path: &str,
) -> Result<(RepoHandle, String, String), Box<dyn std::error::Error>> {
    match mode {
        ServeMode::Single {
            repo_path,
            writer,
            readers,
        } => Ok((
            RepoHandle::new(repo_path.clone(), writer.clone(), readers),
            path.to_owned(),
            "*".to_owned(),
        )),
        ServeMode::Multi { root, cache } => {
            let trimmed = path.trim_start_matches('/');
            let (name, rest) = match trimmed.find('/') {
                Some(i) => (&trimmed[..i], &trimmed[i..]),
                None => (trimmed, ""),
            };
            if let Some(h) = cache.take().get(name) {
                return Ok((h.clone(), rest.to_owned(), name.to_owned()));
            }
            // opened with the cache let go of: an open that waits for a
            // lock of this repository keeps no request for another waiting
            let repo_path = readers::named(root, name)?;
            // Probe once: surfaces "not a repo" errors right at cache
            // insertion rather than on every request.
            Repository::discover(&repo_path)?;
            let alt_dir = repo_path.join(".alt");
            let writer = match alt_dir.is_dir() {
                true => Some(open_write_coordinator(alt_dir)?),
                false => None,
            };
            // the first to have opened it is the one every request uses
            let opened = RepoHandle::new(repo_path, writer, &readers::none());
            let handle = cache
                .take()
                .entry(name.to_owned())
                .or_insert(opened)
                .clone();
            Ok((handle, rest.to_owned(), name.to_owned()))
        }
    }
}

fn dispatch(
    mode: &ServeMode,
    method: Method,
    url: &str,
    req: http::Request,
    log: &mut LogCtx,
) -> Result<(), Box<dyn std::error::Error>> {
    // optional Basic auth in multi-repo mode. When the server
    // root has a `users` file, every request must carry a valid
    // Authorization header; absence / mismatch returns HTTP 401 with a
    // WWW-Authenticate prompt so a real git client retries with creds.
    // a scoped user (3-column line) hands back an ACL the
    // dispatcher checks against the resolved repo + action below.
    let mut auth_user: Option<String> = None;
    let mut scoped_acl: Option<Vec<AclRule>> = None;
    if let ServeMode::Multi { root, .. } = mode {
        let users_path = root.join("users");
        if users_path.is_file() {
            match check_auth(&req, &users_path) {
                AuthOutcome::Allow { user, acl } => {
                    auth_user = user;
                    scoped_acl = acl;
                }
                AuthOutcome::Reject(reason) => {
                    let mut resp = Response::from_string(reason).with_status_code(StatusCode(401));
                    resp.add_header(header("WWW-Authenticate", "Basic realm=\"altd-server\""));
                    respond_logged(req, resp, log)?;
                    return Ok(());
                }
            }
        }
    }
    log.principal = Some(auth_user.clone().unwrap_or_else(|| "anonymous".to_owned()));

    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url, ""),
    };
    // Multi-repo mode peels the first path segment as the repo name and
    // hands back the remaining suffix; single-repo mode keeps the path
    // intact. After resolve_repo, the suffix always ends in one of the
    // smart-http endpoints if the URL was well-formed.
    let (handle, suffix, repo_name) = match resolve_repo(mode, path) {
        Ok(v) => v,
        Err(e) => {
            let r = Response::from_string(e.to_string()).with_status_code(StatusCode(404));
            respond_logged(req, r, log)?;
            return Ok(());
        }
    };
    log.repo = Some(repo_name.clone());

    // gate the resolved request against the scoped user's
    // ACL. Trusted (no-ACL) users skip the check entirely; the request
    // proceeds unscoped.
    if let Some(acl) = &scoped_acl
        && let Some(action) = action_from_request(&method, &suffix, query)
        && !acl_allows(acl, &repo_name, action)
    {
        let r = Response::from_string(format!(
            "forbidden: no {action:?} permission on repo '{repo_name}'"
        ))
        .with_status_code(StatusCode(403));
        respond_logged(req, r, log)?;
        return Ok(());
    }

    // fail-fast 413 when the client *advertised* a body
    // larger than the cap. Lying Content-Length headers are caught
    // at read time inside the handlers via `read_body_capped`.
    if method == Method::Post
        && let Some(advertised) = req.body_length()
        && advertised as u64 > max_body_bytes()
    {
        let max = max_body_bytes();
        let r = Response::from_string(format!(
            "payload too large: Content-Length={advertised} exceeds cap {max}"
        ))
        .with_status_code(StatusCode(413));
        respond_logged(req, r, log)?;
        return Ok(());
    }

    // figure out which (allowed methods, endpoint)
    // the suffix matches first, then check method. A path match with
    // the wrong method returns 405 + Allow header; a real OPTIONS
    // request returns 204 + Allow + (optionally) CORS preflight
    // headers. This replaces the previous blanket-404 path that ate
    // both signals.
    let endpoint_allow: Option<&'static str> = if suffix.ends_with("/info/refs") {
        Some("GET, OPTIONS")
    } else if suffix.ends_with("/git-upload-pack") || suffix.ends_with("/git-receive-pack") {
        Some("POST, OPTIONS")
    } else {
        None
    };

    if method == Method::Options {
        let resp = build_options_response(endpoint_allow);
        respond_logged(req, resp, log)?;
        return Ok(());
    }

    if let Some(allow) = endpoint_allow {
        if method == Method::Get && suffix.ends_with("/info/refs") {
            let repo = handle.open_repo()?;
            return handle_info_refs(&repo, handle.writer.as_deref(), query, req, log);
        }
        if method == Method::Post && suffix.ends_with("/git-upload-pack") {
            let repo = handle.open_repo()?;
            return handle_upload_pack(&repo, req, log);
        }
        if method == Method::Post && suffix.ends_with("/git-receive-pack") {
            let repo = handle.open_repo()?;
            let Some(writer) = handle.writer else {
                let r = Response::from_string("repo is read-only (no .alt write store)")
                    .with_status_code(StatusCode(403));
                respond_logged(req, r, log)?;
                return Ok(());
            };
            return handle_receive_pack(&repo, &writer, auth_user.as_deref(), req, log);
        }
        // Path matched but method didn't — 405 Method Not Allowed.
        let mut r = Response::from_string(format!("method not allowed; allow={allow}"))
            .with_status_code(StatusCode(405));
        r.add_header(header("Allow", allow));
        respond_logged(req, r, log)?;
        return Ok(());
    }

    let r = Response::from_string("not found").with_status_code(StatusCode(404));
    respond_logged(req, r, log)?;
    Ok(())
}

/// preflight + bare OPTIONS responder.
///
/// Returns 204 + `Allow:` listing the methods the matched endpoint
/// supports (or `GET, POST, OPTIONS` when the OPTIONS hit doesn't
/// resolve to a known endpoint — closer to "what the server speaks"
/// than spec-strict but useful as a probe response).
///
/// CORS: opt-in via `ALT_SERVER_CORS_ALLOW_ORIGIN`. Defaults to no
/// `Access-Control-*` headers, which keeps a default-config server
/// off the open-CORS attack surface. Operators who want web UI
/// access set the env to the exact origin (`https://ui.example.com`)
/// or `*` for fully-open dev mode.
fn build_options_response(endpoint_allow: Option<&'static str>) -> Response<Cursor<Vec<u8>>> {
    let allow = endpoint_allow.unwrap_or("GET, POST, OPTIONS");
    let mut resp = Response::from_data(Vec::<u8>::new()).with_status_code(StatusCode(204));
    resp.add_header(header("Allow", allow));
    if let Ok(origin) = std::env::var("ALT_SERVER_CORS_ALLOW_ORIGIN")
        && !origin.is_empty()
    {
        resp.add_header(header("Access-Control-Allow-Origin", &origin));
        resp.add_header(header("Access-Control-Allow-Methods", allow));
        resp.add_header(header(
            "Access-Control-Allow-Headers",
            "Content-Type, Authorization, Git-Protocol, Content-Encoding",
        ));
        resp.add_header(header("Access-Control-Max-Age", "86400"));
    }
    resp
}

/// POST /git-upload-pack. v2 dispatch on the first `command=…` line:
/// `ls-refs` and `fetch` both land here. The request body
/// header is identical (`command=<x>\n` + optional `object-format=…`),
/// only the args section differs — sniff the first frame to route.
fn handle_upload_pack(
    repo: &Repository,
    mut req: http::Request,
    log: &mut LogCtx,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut body = Vec::new();
    if let Err(e) = read_body_capped(&mut req, &mut body, max_command_bytes()) {
        let r = Response::from_string(format!("{e}")).with_status_code(limits::refusal(&*e));
        respond_logged(req, r, log)?;
        return Ok(());
    }
    let cmd = sniff_command(&body)?;

    let mut out = Vec::new();
    match cmd.as_str() {
        "ls-refs" => {
            let (lsr_req, _fmt) =
                alt_wire::ls_refs::parse_ls_refs_request(&mut Cursor::new(&body))?;
            let refs = read_refs(repo, &lsr_req)?;
            alt_wire::ls_refs::encode_ls_refs_response(&mut out, &refs)?;
        }
        "fetch" => {
            let (fetch_req, _fmt) = alt_wire::fetch::parse_fetch_request(&mut Cursor::new(&body))?;
            let mut resp = Response::from_file(fetch::response(repo, &fetch_req)?);
            resp.add_header(header("Content-Type", UPLOAD_PACK_RESULT));
            resp.add_header(header("Cache-Control", "no-cache"));
            return Ok(respond_logged(req, resp, log)?);
        }
        other => {
            let r = Response::from_string(format!("unknown command={other}"))
                .with_status_code(StatusCode(400));
            respond_logged(req, r, log)?;
            return Ok(());
        }
    }

    let mut resp = Response::from_data(out);
    resp.add_header(header("Content-Type", UPLOAD_PACK_RESULT));
    resp.add_header(header("Cache-Control", "no-cache"));
    respond_logged(req, resp, log)?;
    Ok(())
}

/// Find the leading `command=<name>` line of a v2 request body so the
/// dispatch can pick its parser. The body is a stream of pkt-lines; the
/// first data frame after the optional `# service=…` is the command.
fn sniff_command(body: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    let mut r = Cursor::new(body);
    let mut scratch = Vec::new();
    loop {
        match alt_wire::pkt::read_frame(&mut r, &mut scratch)? {
            alt_wire::pkt::Frame::Data(d) => {
                let trimmed = trim_newline(d);
                let s = std::str::from_utf8(trimmed)?;
                if let Some(name) = s.strip_prefix("command=") {
                    return Ok(name.to_owned());
                }
                // skip non-command headers
            }
            alt_wire::pkt::Frame::Delim
            | alt_wire::pkt::Frame::Flush
            | alt_wire::pkt::Frame::ResponseEnd => {
                return Err("v2 request missing command= header".into());
            }
        }
    }
}

fn trim_newline(b: &[u8]) -> &[u8] {
    let mut end = b.len();
    while end > 0 && (b[end - 1] == b'\n' || b[end - 1] == b'\r') {
        end -= 1;
    }
    &b[..end]
}

fn handle_info_refs(
    repo: &Repository,
    writer: Option<&WriteCoordinator>,
    query: &str,
    req: http::Request,
    log: &mut LogCtx,
) -> Result<(), Box<dyn std::error::Error>> {
    let service = parse_query(query, "service")
        .ok_or("info/refs missing ?service= query parameter")?
        .to_owned();
    let body = match service.as_str() {
        // Fetch (read): v2 capability advert. Client posts ls-refs / fetch
        // next over POST /git-upload-pack.
        "git-upload-pack" => {
            let mut body = Vec::new();
            caps::encode_capability_advertisement(
                &mut body,
                &service,
                AGENT,
                Some("sha1"),
                &[
                    ("ls-refs", Some("unborn")),
                    // advertise `filter` so git's partial-clone
                    // path (`--filter=blob:none` etc) negotiates against us
                    ("fetch", Some("shallow wait-for-done filter")),
                ],
            )?;
            body
        }
        // Push (write): receive-pack is still v0/v1 in git. The advert
        // carries the actual ref list inline so the client can compute
        // what to push.
        "git-receive-pack" => {
            let mut body = Vec::new();
            let refs: Vec<(String, ObjectId)> = repo
                .list_refs()?
                .into_iter()
                .filter(|(name, _, _)| name != "HEAD")
                .map(|(name, oid, _)| (name, oid))
                .collect();
            // issue a single-use nonce per advert and attach
            // it as the `alt-nonce=<hex>` capability. A signing alt
            // client will sign `nonce <hex>\n` + canonical payload and
            // echo the same `alt-nonce=<hex>` capability back on its
            // push, so the server can look the nonce up, verify the
            // signature, and consume it. Anyone replaying the same
            // captured push body gets refused on the second attempt.
            //
            // Read-only mode (no writer) doesn't issue nonces; pushes
            // are refused before they ever get verified anyway.
            let nonce_cap: Option<String> = writer.map(|w| {
                let n = w.nonces.issue();
                format!("{}={n}", alt_wire::CAP_ALT_NONCE)
            });
            let mut caps_list: Vec<&str> = vec![
                "report-status",
                "delete-refs",
                "ofs-delta",
                "side-band-64k",
                concat!("agent=", "alt-server/", env!("CARGO_PKG_VERSION")),
            ];
            if let Some(s) = nonce_cap.as_deref() {
                caps_list.push(s);
            }
            alt_wire::push::encode_v1_ref_advertisement(
                &mut body,
                &refs,
                &caps_list,
                HashAlgo::Sha1,
            )?;
            body
        }
        _ => {
            let r = Response::from_string("unknown service").with_status_code(StatusCode(400));
            respond_logged(req, r, log)?;
            return Ok(());
        }
    };
    let content_type = format!("application/x-{service}-advertisement");
    let mut resp = Response::from_data(body);
    resp.add_header(header("Content-Type", &content_type));
    resp.add_header(header("Cache-Control", "no-cache"));
    respond_logged(req, resp, log)?;
    Ok(())
}

/// Reads the head of a push off the connection and streams the pack
/// behind it into the file at `pack`, so that only the head is ever in
/// memory, and a push that is refused for its signature never has its
/// pack looked at. One byte past `max` is read at most: the caller tells
/// a body over the cap by the count. The reader's borrow of `req` ends
/// here, before the response is sent.
fn read_push(
    req: &mut http::Request,
    max: u64,
    pack: &std::path::Path,
) -> Result<(alt_wire::push::PushHead, u64), String> {
    use std::io::Read;
    let gzipped = req
        .headers()
        .iter()
        .any(|h| h.field.equiv("Content-Encoding") && h.value.as_str().contains("gzip"));
    let mut reader: Box<dyn Read> = if gzipped {
        Box::new(alt_inflate::Reader::new(
            alt_inflate::Format::Gzip,
            req.as_reader().take(max + 1),
        ))
    } else {
        Box::new(req.as_reader().take(max + 1))
    };
    // the commands are held in memory: they get a cap of their own
    let mut commands = (&mut reader).take(max_command_bytes());
    let head = alt_wire::push::parse_push_request_head(&mut commands, HashAlgo::Sha1)
        .map_err(|e| format!("{e}"))?;
    let mut tmp = std::fs::File::create(pack).map_err(|e| format!("tempfile: {e}"))?;
    // the cap is on what the body inflates to as well: a gzip body is
    // counted as it is written out. Not synced: the file is gone when
    // the request is over
    let n = std::io::copy(&mut (&mut reader).take(max + 1), &mut tmp)
        .map_err(|e| format!("stream pack: {e}"))?;
    Ok((head, n))
}

/// The `report-status` of a push: how the unpack went and, for each
/// command, whether the refs moved or why they did not.
fn report_status(
    head: &alt_wire::push::PushHead,
    unpack: &Result<(), String>,
    moved: &Result<(), String>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let status = |u: &alt_wire::push::RefUpdate| match moved {
        Ok(()) => CommandStatus::Ok(u.name.clone()),
        Err(reason) => CommandStatus::Ng {
            name: u.name.clone(),
            reason: reason.clone(),
        },
    };
    let command_status: Vec<CommandStatus> = head.updates.iter().map(status).collect();
    let unpack = unpack.as_ref().map(|_| ()).map_err(|s| s.as_str());
    let mut out = Vec::new();
    match head.capabilities.iter().any(|c| c == "side-band-64k") {
        true => alt_wire::push::encode_report_status_sideband(&mut out, unpack, &command_status)?,
        false => alt_wire::push::encode_report_status(&mut out, unpack, &command_status)?,
    }
    Ok(out)
}

/// POST /git-receive-pack: parse the client's ref-update list
/// and raw pack, ingest objects into the alt odb, then commit the ref
/// changes through `RefStore::commit` so the whole push lands as one
/// atomic op-log entry (mirrors the local-commit path). Reply with a
/// `report-status` body.
fn handle_receive_pack(
    repo: &Repository,
    writer: &WriteCoordinator,
    auth_user: Option<&str>,
    mut req: http::Request,
    log: &mut LogCtx,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = &writer.store;
    let max = max_body_bytes();
    let pack_tmp_dir = alt_os::temp::tempdir()?;
    let pack_tmp_path = pack_tmp_dir.path().join("incoming.pack");
    let parse_outcome = read_push(&mut req, max, &pack_tmp_path);
    let (head, pack_bytes_streamed) = match parse_outcome {
        Ok(t) => t,
        Err(reason) => {
            let r = Response::from_string(reason).with_status_code(StatusCode(400));
            respond_logged(req, r, log)?;
            return Ok(());
        }
    };

    // refresh the cached Store so an operator-edited
    // `.alt/policy` (or fresh oplog ops from another writer) takes
    // effect on the *next* request — no server restart. `users` is
    // already re-read per request inside `check_auth`; `trust` keys
    // are re-read per request inside `verify_push_signature`. policy
    // is the last cached piece, and `Store::refresh()` is the same
    // entry the daemon uses to catch up its read view. Must
    // happen *before* the sig_block decision below so the next
    // signing policy is seen on this request, not a request later.
    if let Err(e) = store.take().settle_told() {
        eprintln!("altd-server: store refresh failed (continuing with stale policy): {e}");
    }

    // pre-commit verify of the wire signature. The
    // signature is computed over the canonical push payload (head's
    // updates + algo); pack bytes don't participate, so we can decide
    // sig_block before reading them.
    let sig_outcome = verify_push_signature(writer, &head)?;
    let principal = pick_principal(&sig_outcome, auth_user);
    let require_signed = require_signed_for(store, &principal.0);
    let sig_block: Option<String> = match (&sig_outcome, require_signed) {
        (SigOutcome::Verified { .. }, _) => None,
        (other, true) => Some(format!("signature required: {}", describe_sig(other))),
        (_, false) => None,
    };

    if pack_bytes_streamed > max {
        let r = Response::from_string(format!("payload too large: pack body exceeds cap {max}"))
            .with_status_code(StatusCode(413));
        respond_logged(req, r, log)?;
        return Ok(());
    }

    let pack = (pack_bytes_streamed != 0).then(|| (pack_tmp_path.as_path(), pack_limits()));
    // nothing is appended behind what a failed sync left in doubt
    let stopped = writer.group.failed().map(|e| format!("{STOPPED}: {e}"));
    let (unpack_result, moved) = match sig_block.or(stopped) {
        Some(reason) => (Ok(()), Err(reason)),
        None => receive::take_in(store, pack, &head.updates, &principal, &writer.group)?,
    };
    // wait for the group-commit leader's fsync to cover us, outside the
    // store lock: this is the only place we touch disk durability on the
    // receive-pack path. Other concurrent pushes coalesce into the same
    // flush, so 4-way concurrent traffic pays ~1 fsync instead of 4.
    let moved = moved.and_then(|ticket| {
        let durable = match ticket {
            Some(t) => writer.group.await_durable(&writer.sink, t),
            None => Ok(()),
        };
        log.fsync_seq = Some(writer.group.fsync_count());
        durable.map_err(|e| format!("durability: {e}"))
    });
    let out = report_status(&head, &unpack_result, &moved)?;
    let mut resp = Response::from_data(out);
    resp.add_header(header(
        "Content-Type",
        "application/x-git-receive-pack-result",
    ));
    resp.add_header(header("Cache-Control", "no-cache"));
    respond_logged(req, resp, log)?;
    let _ = repo; // borrow keepalive across response
    Ok(())
}

/// result of verifying the `alt-principal=<id>` +
/// `alt-sig=<ed25519>` capabilities the client may have attached to a
/// push. `NoSignature` is the empty-attribution baseline; the rest are
/// failure modes the policy gate can act on.
#[derive(Debug)]
enum SigOutcome {
    Verified { principal_id: String },
    NoSignature,
    BadSignature(String),
    UnknownPrincipal(String),
}

fn describe_sig(o: &SigOutcome) -> String {
    match o {
        SigOutcome::Verified { .. } => "verified".into(),
        SigOutcome::NoSignature => "no signature attached".into(),
        SigOutcome::BadSignature(e) => format!("signature did not verify ({e})"),
        SigOutcome::UnknownPrincipal(id) => format!("principal '{id}' not in trust store"),
    }
}

/// Run the signature check against `<alt-dir>/trust/`. Returns
/// `NoSignature` when the client didn't attach the pair (the common
/// path on git-native clients).
fn verify_push_signature(
    writer: &WriteCoordinator,
    head: &alt_wire::push::PushHead,
) -> Result<SigOutcome, Box<dyn std::error::Error>> {
    let principal_id = head
        .capabilities
        .iter()
        .find_map(|c| c.strip_prefix(&format!("{}=", alt_wire::CAP_ALT_PRINCIPAL)));
    let sig_text = head
        .capabilities
        .iter()
        .find_map(|c| c.strip_prefix(&format!("{}=", alt_wire::CAP_ALT_SIG)));
    let (principal_id, sig_text) = match (principal_id, sig_text) {
        (Some(p), Some(s)) => (p.to_owned(), s.to_owned()),
        _ => return Ok(SigOutcome::NoSignature),
    };
    let trust = {
        let guard = writer.store.take();
        guard.trust_keys()?
    };
    let Some((_, pubkey)) = trust.iter().find(|(id, _)| id == &principal_id) else {
        return Ok(SigOutcome::UnknownPrincipal(principal_id));
    };
    let sig = match alt_sign::Sig::from_text(&sig_text) {
        Ok(s) => s,
        Err(e) => return Ok(SigOutcome::BadSignature(format!("{e}"))),
    };
    // if the client echoed back an `alt-nonce=<hex>` cap, the
    // signature is over `nonce <hex>\n` + canonical_payload. We must
    // consume the nonce (single-use, anti-replay) and verify against
    // the nonce-prefixed payload. No nonce echo = legacy payload,
    // verified against the no-nonce form for backwards-compat with
    // pre-nonce clients (the `require_nonce_on_sig` policy axis turns
    // that compat off).
    let echoed_nonce = head
        .capabilities
        .iter()
        .find_map(|c| c.strip_prefix(&format!("{}=", alt_wire::CAP_ALT_NONCE)));
    if let Some(nonce) = echoed_nonce {
        if !writer.nonces.consume(nonce) {
            return Ok(SigOutcome::BadSignature(
                "alt-nonce echoed in push not active on this server (replay or expired)".into(),
            ));
        }
        let payload =
            alt_wire::canonical_push_payload_with_nonce(&head.updates, Some(nonce), HashAlgo::Sha1);
        match pubkey.verify(&payload, &sig) {
            Ok(()) => Ok(SigOutcome::Verified { principal_id }),
            Err(e) => Ok(SigOutcome::BadSignature(format!("{e}"))),
        }
    } else {
        let payload = alt_wire::canonical_push_payload(&head.updates, HashAlgo::Sha1);
        match pubkey.verify(&payload, &sig) {
            Ok(()) => Ok(SigOutcome::Verified { principal_id }),
            Err(e) => Ok(SigOutcome::BadSignature(format!("{e}"))),
        }
    }
}

/// Pick the effective principal + signature label for the op-log actor
/// string. A verified signature wins over Basic-auth (it's a stronger
/// claim of identity); otherwise we fall back to the Basic-auth user
/// (or "anonymous") and the label records *why* it isn't verified.
fn pick_principal(outcome: &SigOutcome, auth_user: Option<&str>) -> (Principal, &'static str) {
    let (id, label) = match outcome {
        SigOutcome::Verified { principal_id } => (principal_id.clone(), "ed25519"),
        SigOutcome::NoSignature => (auth_user.unwrap_or("anonymous").to_owned(), "none"),
        SigOutcome::BadSignature(_) => (auth_user.unwrap_or("anonymous").to_owned(), "bad"),
        SigOutcome::UnknownPrincipal(_) => (
            auth_user.unwrap_or("anonymous").to_owned(),
            "unknown-principal",
        ),
    };
    (
        Principal {
            kind: PrincipalKind::Human,
            id,
            session: None,
        },
        label,
    )
}

/// Does this principal's policy require a verified signature on every
/// push? If so, an unverified push is short-circuited with `ng
/// signature required` before any objects land.
fn require_signed_for(store: &Held<Store>, principal: &Principal) -> bool {
    let guard = store.take();
    guard.capabilities_for(principal).require_signed
}

/// Read the repo's refs into the ls-refs `RefRecord` shape the wire
/// encoder consumes. Used by the POST /git-upload-pack handler.
fn read_refs(
    repo: &Repository,
    req: &alt_wire::ls_refs::LsRefsRequest,
) -> Result<Vec<RefRecord>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for (name, oid, sym) in repo.list_refs()? {
        if !req.ref_prefixes.is_empty() && !req.ref_prefixes.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let symref_target = if req.symrefs { sym } else { None };
        out.push(RefRecord {
            oid,
            name,
            symref_target,
            peeled: None,
            other: Default::default(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn parse_query<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    for chunk in query.split('&') {
        if let Some((k, v)) = chunk.split_once('=')
            && k == key
        {
            return Some(v);
        }
    }
    None
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static header literals are valid")
}

fn die(msg: &str) -> ! {
    eprintln!("altd-server: {msg}");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the table issues a nonce that consumes exactly once;
    /// the second consume on the same value returns false. That's
    /// the single-use anti-replay primitive.
    #[test]
    fn nonce_table_consume_is_single_use() {
        let t = NonceTable::new();
        let n = t.issue();
        assert_eq!(n.len(), 32, "issued nonce should be 32 hex chars");
        assert!(t.consume(&n), "first consume must succeed");
        assert!(!t.consume(&n), "second consume on the same nonce must fail");
    }

    /// A nonce that was never issued can't be consumed. Stops a
    /// confused-deputy that tries to pass off a guessed value as
    /// if it had been negotiated.
    #[test]
    fn nonce_table_rejects_never_issued_value() {
        let t = NonceTable::new();
        assert!(
            !t.consume("00000000000000000000000000000000"),
            "consuming a never-issued nonce must fail"
        );
    }

    /// At cap (1024 entries), issuing one more evicts the oldest;
    /// after eviction the evicted value can't be consumed. This
    /// bounds memory regardless of traffic at the cost of refusing
    /// an honest client whose info/refs nonce aged out before they
    /// posted receive-pack.
    #[test]
    fn nonce_table_evicts_oldest_when_full() {
        let t = NonceTable::new();
        let first = t.issue();
        for _ in 1..1024 {
            let _ = t.issue();
        }
        // table is full now; one more issue must evict `first`.
        let _last = t.issue();
        assert!(
            !t.consume(&first),
            "the evicted (oldest) nonce must no longer be consumable"
        );
    }
}
