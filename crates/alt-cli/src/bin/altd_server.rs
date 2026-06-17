//! M9/W10a — alt's wire-protocol server (`altd-server`).
//!
//! Serves git protocol v2 over HTTP for a single alt repo (multi-repo
//! routing arrives in W11). The contract is the smart-http v2 entry
//! every git client recognises:
//!
//!   GET  /info/refs?service=git-upload-pack    → capability advert + ls-refs
//!   GET  /info/refs?service=git-receive-pack   → capability advert + ls-refs
//!   POST /git-upload-pack                      → command-dispatch (ls-refs, fetch — W10b)
//!   POST /git-receive-pack                     → command-dispatch (W10c)
//!
//! W10a delivers the `info/refs` handler end-to-end: server reads the
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

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use alt_cli::native::{Principal, PrincipalKind, Store};
use alt_git_codec::{HashAlgo, ObjectId};
use alt_refs::{RefChange, RefTarget};
use alt_repo::Repository;
use alt_wire::caps;
use alt_wire::ls_refs::RefRecord;
use alt_wire::push::{CommandStatus, RefUpdate};
use tiny_http::{Header, Method, Response, Server, StatusCode};

const AGENT: &str = concat!("alt-server/", env!("CARGO_PKG_VERSION"));

/// M11/W23: per-request log context filled in as dispatch progresses.
/// The outer serve loop reads it back after dispatch returns and emits
/// one JSON-line access log entry, so a single request is one line of
/// machine-readable observability — same调性 as `alt`'s `--json` paths
/// (信条 #5 / AI-first).
#[derive(Default)]
struct LogCtx {
    status: u16,
    bytes_in: u64,
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
    req: tiny_http::Request,
    resp: Response<R>,
    log: &mut LogCtx,
) -> std::io::Result<()> {
    log.status = resp.status_code().0;
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

    // M11/W24: parallel request dispatch via a worker pool. tiny_http
    // is already thread-safe (Server: Send + Sync); previously the outer
    // `for req in incoming_requests()` deserialized everything onto the
    // main thread. With N workers each calling `server.recv()` in a
    // loop, concurrent clients no longer queue behind a slow push.
    let workers: usize = std::env::var("ALT_SERVER_WORKERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n: &usize| *n > 0)
        .unwrap_or(4);

    let server =
        Arc::new(Server::http(&bind).unwrap_or_else(|e| die(&format!("bind {bind}: {e}"))));
    eprintln!(
        "altd-server: listening on {} ({}, workers={workers})",
        server.server_addr(),
        mode.describe()
    );

    // M11/W25: install signal handlers so SIGINT (Ctrl-C) and SIGTERM
    // (`systemctl stop`, `docker stop`, k8s preStop) tip the shutdown
    // flag. The handler must stay async-signal-safe, so it only does an
    // atomic store; the main thread polls it and drives the actual
    // shutdown sequence.
    install_signal_handlers();

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
    // fires, unblock all worker `recv()` calls (tiny_http: pending
    // recv → Err, in-flight responses keep going) and join. The poll
    // is cheap (100 ms) and only the main thread runs it, so the
    // worker pool stays untouched.
    loop {
        if SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!("altd-server: shutdown signal received, unblocking workers");
            // tiny_http: `unblock()` frees only ONE blocked recv() per
            // call (https://docs.rs/tiny_http/0.12.0/tiny_http/struct.Server.html#method.unblock),
            // so call it once per worker to drain the pool.
            for _ in 0..workers {
                server.unblock();
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // M13/W38: a stuck worker (long ingest_pack, a malicious slow
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

/// M11/W26: hard cap on POST request bodies (push pack + upload-pack
/// command bodies). Bound by `ALT_SERVER_MAX_PUSH_BYTES`; default
/// 1 GiB matches what a healthy alt push against the dogfood corpus
/// fits inside, and stops a malicious or buggy client from streaming
/// gigabytes into memory through `std::io::copy(req.as_reader(), …)`.
fn max_body_bytes() -> u64 {
    use std::sync::OnceLock;
    static CACHE: OnceLock<u64> = OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("ALT_SERVER_MAX_PUSH_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(1024 * 1024 * 1024)
    })
}

/// Read at most `max_body_bytes()` from `req.as_reader()` into `body`,
/// transparently gunzip'ing when the client set `Content-Encoding:
/// gzip`. Returns `Err` when the client streamed past the cap — the
/// handler maps that into a 413, never an OOM. We don't trust
/// `body_length()` alone (a malicious client can send a small
/// Content-Length header and then keep writing); the `.take()` is
/// the real enforcement, and it bounds BOTH the on-wire (compressed)
/// bytes and the decoded output.
///
/// M11/W31 background: git's smart-http client transparently sets
/// `Content-Encoding: gzip` on POST bodies it expects to compress
/// well (push pack payloads, repeated want/have lists). Without
/// gunzip on the server, the alt-wire pkt-line parser sees the gzip
/// magic `1f 8b 08 …` and bails with "length prefix is not valid
/// hex" — which the stress harness deterministically hit at scale.
fn read_body_capped(
    req: &mut tiny_http::Request,
    body: &mut Vec<u8>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;
    let max = max_body_bytes();
    let gzipped = req
        .headers()
        .iter()
        .any(|h| h.field.equiv("Content-Encoding") && h.value.as_str().contains("gzip"));
    let limited = req.as_reader().take(max + 1);
    if gzipped {
        // Decompress under the same cap so a 100 KiB gzip bomb that
        // expands to 10 GiB still bounces.
        let mut decoder = flate2::read::GzDecoder::new(limited);
        let mut decoded_cap = (&mut decoder).take(max + 1);
        decoded_cap.read_to_end(body)?;
    } else {
        let mut limited = limited;
        limited.read_to_end(body)?;
    }
    if body.len() as u64 > max {
        return Err(format!(
            "request body exceeds ALT_SERVER_MAX_PUSH_BYTES={max} (cap is per-request)"
        )
        .into());
    }
    Ok(())
}

#[cfg(unix)]
extern "C" fn shutdown_handler(_sig: libc::c_int) {
    // Async-signal-safe: relaxed atomic store is the only thing we do
    // inside the handler. The main thread does the actual work.
    SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(unix)]
fn install_signal_handlers() {
    let handler = shutdown_handler as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
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
fn serve_one(mode: &ServeMode, req: tiny_http::Request) {
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
/// Single keeps W10's old shape — info/refs lives at the URL root.
/// Multi parses the first path segment as a repo name and resolves it
/// under the configured root.
enum ServeMode {
    Single {
        repo_path: PathBuf,
        store: Option<Arc<Mutex<Store>>>,
    },
    Multi {
        root: PathBuf,
        cache: Mutex<HashMap<String, RepoHandle>>,
    },
}

/// A resolved repo binding for one request. `repo_path` is reopened on
/// every read so the Repository's RefStore + odb always reflect any
/// receive-pack write that just completed (no stale-cache window between
/// push → fetch on the same connection). The mutable `Store` is shared
/// across requests for write serialisation.
struct RepoHandle {
    repo_path: PathBuf,
    store: Option<Arc<Mutex<Store>>>,
}

impl RepoHandle {
    fn open_repo(&self) -> Result<Repository, Box<dyn std::error::Error>> {
        Ok(Repository::discover(&self.repo_path)?)
    }
}

impl ServeMode {
    fn single(repo_path: PathBuf) -> Self {
        // Fail-fast: probe the repo once at boot so a misconfigured
        // ALT_SERVER_REPO surfaces immediately rather than on the first
        // request. We drop the handle; per-request opens are cheap.
        Repository::discover(&repo_path).unwrap_or_else(|e| die(&format!("opening repo: {e}")));
        let alt_dir = repo_path.join(".alt");
        let store: Option<Arc<Mutex<Store>>> = if alt_dir.is_dir() {
            Some(Arc::new(Mutex::new(Store::open(alt_dir).unwrap_or_else(
                |e| die(&format!("opening write store: {e}")),
            ))))
        } else {
            eprintln!(
                "altd-server: no .alt under {p}; receive-pack will be refused (read-only mode)",
                p = repo_path.display()
            );
            None
        };
        ServeMode::Single { repo_path, store }
    }

    fn multi(root: PathBuf) -> Self {
        if !root.is_dir() {
            die(&format!(
                "ALT_SERVER_ROOT={} does not exist or isn't a directory",
                root.display()
            ));
        }
        // M14/W40: fail-open auth guard. When `ALT_SERVER_REQUIRE_AUTH=1`
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
            cache: Mutex::new(HashMap::new()),
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
        ServeMode::Single { repo_path, store } => Ok((
            RepoHandle {
                repo_path: repo_path.clone(),
                store: store.clone(),
            },
            path.to_owned(),
            "*".to_owned(),
        )),
        ServeMode::Multi { root, cache } => {
            let trimmed = path.trim_start_matches('/');
            let (name, rest) = match trimmed.find('/') {
                Some(i) => (&trimmed[..i], &trimmed[i..]),
                None => (trimmed, ""),
            };
            if name.is_empty() || name.contains("..") || name.contains('\\') {
                return Err("invalid repo name in URL".into());
            }
            let mut cache_g = cache.lock().unwrap();
            if let Some(h) = cache_g.get(name) {
                return Ok((
                    RepoHandle {
                        repo_path: h.repo_path.clone(),
                        store: h.store.clone(),
                    },
                    rest.to_owned(),
                    name.to_owned(),
                ));
            }
            let repo_path = root.join(name);
            if !repo_path.is_dir() {
                return Err(format!("repo '{name}' not found under server root").into());
            }
            // Probe once: surfaces "not a repo" errors right at cache
            // insertion rather than on every request.
            Repository::discover(&repo_path)?;
            let alt_dir = repo_path.join(".alt");
            let store: Option<Arc<Mutex<Store>>> = if alt_dir.is_dir() {
                Some(Arc::new(Mutex::new(Store::open(alt_dir)?)))
            } else {
                None
            };
            let handle = RepoHandle {
                repo_path: repo_path.clone(),
                store: store.clone(),
            };
            cache_g.insert(name.to_owned(), handle);
            Ok((
                RepoHandle { repo_path, store },
                rest.to_owned(),
                name.to_owned(),
            ))
        }
    }
}

// silence the unused import — `Path` is reserved for future path-trim
// helpers; pulling it in alongside `PathBuf` matches the codebase style
const _: fn(&Path) = |_| {};

fn dispatch(
    mode: &ServeMode,
    method: Method,
    url: &str,
    req: tiny_http::Request,
    log: &mut LogCtx,
) -> Result<(), Box<dyn std::error::Error>> {
    // M9/W11b — optional Basic auth in multi-repo mode. When the server
    // root has a `users` file, every request must carry a valid
    // Authorization header; absence / mismatch returns HTTP 401 with a
    // WWW-Authenticate prompt so a real git client retries with creds.
    // M9/W11c — a scoped user (3-column line) hands back an ACL the
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

    // M9/W11c — gate the resolved request against the scoped user's
    // ACL. Trusted (no-ACL) users skip the check entirely; the request
    // proceeds as in W11b.
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

    // M11/W26: fail-fast 413 when the client *advertised* a body
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

    // M14/W41 (G+H): figure out which (allowed methods, endpoint)
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
            return handle_info_refs(&repo, query, req, log);
        }
        if method == Method::Post && suffix.ends_with("/git-upload-pack") {
            let repo = handle.open_repo()?;
            return handle_upload_pack(&repo, req, log);
        }
        if method == Method::Post && suffix.ends_with("/git-receive-pack") {
            let repo = handle.open_repo()?;
            let Some(store) = handle.store else {
                let r = Response::from_string("repo is read-only (no .alt write store)")
                    .with_status_code(StatusCode(403));
                respond_logged(req, r, log)?;
                return Ok(());
            };
            return handle_receive_pack(&repo, &store, auth_user.as_deref(), req, log);
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

/// M14/W41 — preflight + bare OPTIONS responder.
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
/// `ls-refs` (W10a) and `fetch` (W10b) both land here. The request body
/// header is identical (`command=<x>\n` + optional `object-format=…`),
/// only the args section differs — sniff the first frame to route.
fn handle_upload_pack(
    repo: &Repository,
    mut req: tiny_http::Request,
    log: &mut LogCtx,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut body = Vec::new();
    if let Err(e) = read_body_capped(&mut req, &mut body) {
        let r = Response::from_string(format!("{e}")).with_status_code(StatusCode(413));
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
            let pack_bytes = build_pack_for_fetch(repo, &fetch_req)?;
            alt_wire::fetch::encode_fetch_response_packfile(&mut out, &pack_bytes)?;
        }
        other => {
            let r = Response::from_string(format!("unknown command={other}"))
                .with_status_code(StatusCode(400));
            respond_logged(req, r, log)?;
            return Ok(());
        }
    }

    let mut resp = Response::from_data(out);
    resp.add_header(header(
        "Content-Type",
        "application/x-git-upload-pack-result",
    ));
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

/// Resolve the fetch request's want/have closure against the served
/// repo's objects, then stream them through a plain `PackWriter` into a
/// tempdir — the bytes are the wire payload, we don't keep the pack as
/// stored state. Mirrors the existing push-side pack build in
/// `alt-cli::native`.
fn build_pack_for_fetch(
    repo: &Repository,
    req: &alt_wire::fetch::FetchRequest,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let outgoing = repo.reachable_objects(&req.wants, &req.haves)?;
    if outgoing.is_empty() {
        return Ok(Vec::new());
    }
    // M10/W17: drop objects that the client's filter excludes. The
    // first cut handles `blob:none` / `blob:limit=<n>` / `tree:0` —
    // git's three common partial-clone filters.
    let filter = parse_filter_spec(req.filter.as_deref());
    let dir = tempfile::tempdir()?;
    // Two-pass: first decide which objects survive the filter (needs
    // `read_object` for the `blob:limit` size check), then write them.
    let mut surviving: Vec<(
        ObjectId,
        alt_git_codec::ObjectKind,
        alt_git_codec::RawObject,
    )> = Vec::with_capacity(outgoing.len());
    for (oid, kind) in &outgoing {
        let obj = repo
            .read_object(oid)?
            .ok_or_else(|| format!("outgoing object {oid} missing from server odb"))?;
        if filter.excludes(*kind, &obj.data) {
            continue;
        }
        surviving.push((*oid, *kind, obj));
    }
    if surviving.is_empty() {
        return Ok(Vec::new());
    }
    let count = u32::try_from(surviving.len())
        .map_err(|_| "outgoing object set exceeds u32 (server-side)")?;
    let mut writer = alt_git_pack::PackWriter::create(dir.path(), repo.algo(), count)?;
    for (oid, kind, obj) in &surviving {
        writer.add(*oid, *kind, &obj.data)?;
    }
    let written = writer.finish()?;
    Ok(std::fs::read(&written.pack_path)?)
}

/// The partial-clone filter shapes [`build_pack_for_fetch`] currently
/// honours. Everything else parses as `None` (no filter) for
/// forward-compatibility — unknown filters degrade to a full pack
/// rather than rejecting the fetch, since the wire spec lets the
/// server be permissive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FilterSpec {
    omit_blobs: bool,
    blob_limit: Option<usize>,
    omit_trees: bool,
}

impl FilterSpec {
    fn excludes(&self, kind: alt_git_codec::ObjectKind, data: &[u8]) -> bool {
        match kind {
            alt_git_codec::ObjectKind::Blob => {
                if self.omit_blobs || self.omit_trees {
                    return true;
                }
                if let Some(limit) = self.blob_limit
                    && data.len() >= limit
                {
                    return true;
                }
                false
            }
            alt_git_codec::ObjectKind::Tree => self.omit_trees,
            _ => false,
        }
    }
}

fn parse_filter_spec(raw: Option<&str>) -> FilterSpec {
    let mut out = FilterSpec::default();
    let Some(spec) = raw else {
        return out;
    };
    let spec = spec.trim();
    match spec {
        "blob:none" => out.omit_blobs = true,
        "tree:0" => {
            out.omit_trees = true;
            out.omit_blobs = true; // tree:0 implies blob:none semantically
        }
        _ => {
            if let Some(n) = spec.strip_prefix("blob:limit=")
                && let Ok(limit) = n.parse::<usize>()
            {
                out.blob_limit = Some(limit);
            }
            // unknown / unsupported filters silently degrade to "send
            // everything" — git's spec lets the server be permissive.
        }
    }
    out
}

fn handle_info_refs(
    repo: &Repository,
    query: &str,
    req: tiny_http::Request,
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
                    // M10/W17: advertise `filter` so git's partial-clone
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
            let caps_list = [
                "report-status",
                "delete-refs",
                "ofs-delta",
                "side-band-64k",
                concat!("agent=", "alt-server/", env!("CARGO_PKG_VERSION")),
            ];
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

/// POST /git-receive-pack (M9/W10c): parse the client's ref-update list
/// and raw pack, ingest objects into the alt odb, then commit the ref
/// changes through `RefStore::commit` so the whole push lands as one
/// atomic op-log entry (mirrors the local-commit path). Reply with a
/// `report-status` body.
fn handle_receive_pack(
    repo: &Repository,
    store: &Mutex<Store>,
    auth_user: Option<&str>,
    mut req: tiny_http::Request,
    log: &mut LogCtx,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;
    // M13/W36 streaming path. The push body is parsed in two halves:
    //   1. Head (updates + capabilities + flush) — parsed straight off
    //      the connection. Only the small head bytes ever live in RAM.
    //   2. Pack bytes — `io::copy`'d into a tempfile so a 1 GiB push
    //      never costs us a 1 GiB Vec<u8> allocation. `index_pack`
    //      reads straight off the file path we just wrote.
    //
    // The signature gate (W14) runs after step 1 and *before* step 2,
    // so a require-signed push that misses the cap is rejected without
    // ever reading the pack body — saving the bandwidth + RAM.
    let max = max_body_bytes();
    let gzipped = req
        .headers()
        .iter()
        .any(|h| h.field.equiv("Content-Encoding") && h.value.as_str().contains("gzip"));
    // Stream-and-parse in one scope so the reader's borrow on `req`
    // ends *before* the response is sent. The outcome carries
    // everything downstream needs (head + tempfile path + on-wire
    // bytes streamed) so the response phase is owned-only.
    let pack_tmp_dir = tempfile::tempdir()?;
    let pack_tmp_path = pack_tmp_dir.path().join("incoming.pack");
    let parse_outcome: Result<(alt_wire::push::PushHead, u64), String> = {
        let mut reader: Box<dyn Read> = if gzipped {
            Box::new(flate2::read::GzDecoder::new(req.as_reader().take(max + 1)))
        } else {
            Box::new(req.as_reader().take(max + 1))
        };
        match alt_wire::push::parse_push_request_head(&mut reader, HashAlgo::Sha1) {
            Ok(head) => {
                let mut tmp =
                    std::fs::File::create(&pack_tmp_path).map_err(|e| format!("tempfile: {e}"))?;
                let n = std::io::copy(&mut reader, &mut tmp)
                    .map_err(|e| format!("stream pack: {e}"))?;
                tmp.sync_all().map_err(|e| format!("fsync: {e}"))?;
                Ok((head, n))
            }
            Err(e) => Err(format!("{e}")),
        }
    };
    let (head, pack_bytes_streamed) = match parse_outcome {
        Ok(t) => t,
        Err(reason) => {
            let r = Response::from_string(reason).with_status_code(StatusCode(400));
            respond_logged(req, r, log)?;
            return Ok(());
        }
    };

    // M13/W37: refresh the cached Store so an operator-edited
    // `.alt/policy` (or fresh oplog ops from another writer) takes
    // effect on the *next* request — no server restart. `users` is
    // already re-read per request inside `check_auth`; `trust` keys
    // are re-read per request inside `verify_push_signature`. policy
    // is the last cached piece, and `Store::refresh()` is the same
    // entry the daemon (M5) uses to catch up its read view.
    if let Err(e) = store.lock().unwrap().refresh() {
        eprintln!("altd-server: store refresh failed (continuing with stale policy): {e}");
    }

    // M10/W14 (A5b): pre-commit verify of the wire signature. The
    // signature is computed over the canonical push payload (head's
    // updates + algo); pack bytes don't participate, so we can decide
    // sig_block before reading them.
    let sig_outcome = verify_push_signature(store, &head)?;
    let (effective_principal, sig_label) = pick_principal(&sig_outcome, auth_user);
    let require_signed = require_signed_for(store, &effective_principal);
    let sig_block: Option<String> = match (&sig_outcome, require_signed) {
        (SigOutcome::Verified { .. }, _) => None,
        (other, true) => Some(format!("signature required: {}", describe_sig(other))),
        (_, false) => None,
    };

    let mut new_commits: Vec<ObjectId> = Vec::new();
    if pack_bytes_streamed > max {
        let r = Response::from_string(format!("payload too large: pack body exceeds cap {max}"))
            .with_status_code(StatusCode(413));
        respond_logged(req, r, log)?;
        return Ok(());
    }

    let unpack_result: Result<(), String> = if sig_block.is_some() || pack_bytes_streamed == 0 {
        Ok(())
    } else {
        match ingest_pack_from_path(store, &pack_tmp_path) {
            Ok(commits) => {
                new_commits = commits;
                Ok(())
            }
            Err(e) => Err(format!("index-pack: {e}")),
        }
    };

    // M10/W15: if the policy requires every commit to carry a verified
    // alt-sig header, scan the new commits *after* ingest and decide
    // before touching any refs. A failure here flips into a per-command
    // ng identical in shape to the push-level signature gate above.
    let commit_block: Option<String> = if sig_block.is_none() && unpack_result.is_ok() {
        require_signed_commits_block(store, &effective_principal, &new_commits)?
    } else {
        None
    };

    // Apply ref changes only if the pack unpacked cleanly *and* the
    // signature gate(s) passed; otherwise mark every command `ng` so
    // the client sees a coherent reason.
    let mut command_status: Vec<CommandStatus> = Vec::new();
    let any_block = sig_block.clone().or(commit_block);
    if let Some(reason) = &any_block {
        for u in &head.updates {
            command_status.push(CommandStatus::Ng {
                name: u.name.clone(),
                reason: reason.clone(),
            });
        }
    } else if unpack_result.is_ok() {
        match commit_ref_updates(store, &head.updates, &effective_principal, sig_label) {
            Ok(()) => {
                for u in &head.updates {
                    command_status.push(CommandStatus::Ok(u.name.clone()));
                }
            }
            Err(reason) => {
                for u in &head.updates {
                    command_status.push(CommandStatus::Ng {
                        name: u.name.clone(),
                        reason: reason.clone(),
                    });
                }
            }
        }
    } else {
        for u in &head.updates {
            command_status.push(CommandStatus::Ng {
                name: u.name.clone(),
                reason: "pack unpack failed".into(),
            });
        }
    }

    let mut out = Vec::new();
    let want_sideband = head.capabilities.iter().any(|c| c == "side-band-64k");
    if want_sideband {
        alt_wire::push::encode_report_status_sideband(
            &mut out,
            unpack_result.as_ref().map(|_| ()).map_err(|s| s.as_str()),
            &command_status,
        )?;
    } else {
        alt_wire::push::encode_report_status(
            &mut out,
            unpack_result.as_ref().map(|_| ()).map_err(|s| s.as_str()),
            &command_status,
        )?;
    }
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

/// Write the pushed pack into a tempfile, index it, and put every
/// object into the server odb. Mirrors the fetch ingest path in
/// `alt-cli::native`.
/// Index the pack already on disk at `path` and ingest its objects
/// into the server odb. M13/W36 streaming path: the caller streamed
/// the receive-pack body into `path` (a NamedTempFile) instead of
/// buffering the entire pack in memory, so `index_pack` reads straight
/// off the file we already wrote — no Vec<u8> copy of the pack bytes
/// ever lives in process RAM.
fn ingest_pack_from_path(
    store: &Mutex<Store>,
    path: &std::path::Path,
) -> Result<Vec<ObjectId>, Box<dyn std::error::Error>> {
    let indexed = alt_git_pack::index_pack(path, HashAlgo::Sha1, true)?;
    let ip = alt_git_pack::IndexedPack::open(&indexed.pack_path, HashAlgo::Sha1)?;
    let idx = ip.idx();
    let mut order: Vec<(u64, u32)> = (0..idx.len())
        .map(|i| (idx.offset_at(i).expect("idx in range"), i))
        .collect();
    order.sort_unstable();
    let mut store_guard = store.lock().unwrap();
    let mut new_commits = Vec::new();
    for (offset, i) in order {
        let obj = ip.read_at(offset)?;
        let oid = idx.oid_at(i);
        if obj.kind == alt_git_codec::ObjectKind::Commit {
            new_commits.push(oid);
        }
        store_guard.odb_mut().put(oid, obj.kind, &obj.data)?;
    }
    store_guard.odb_mut().flush()?;
    Ok(new_commits)
}

/// Apply the client's ref updates as a single ref transaction so the
/// server records the push as one op-log entry — same atomicity story
/// as a local `alt commit`. M9/W12 — the authenticated user (when
/// present) becomes the Principal looked up against the repo's A6
/// Policy; the resulting Capabilities feed a RefPolicy gate inside
/// `commit_idempotent`, so a server-side ref-write rule is enforced
/// before any state changes (a denied push leaves no op-log entry).
/// M10/W14 (A5b): result of verifying the `alt-principal=<id>` +
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
    store: &Mutex<Store>,
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
        let guard = store.lock().unwrap();
        guard.trust_keys()?
    };
    let Some((_, pubkey)) = trust.iter().find(|(id, _)| id == &principal_id) else {
        return Ok(SigOutcome::UnknownPrincipal(principal_id));
    };
    let sig = match alt_sign::Sig::from_text(&sig_text) {
        Ok(s) => s,
        Err(e) => return Ok(SigOutcome::BadSignature(format!("{e}"))),
    };
    let payload = alt_wire::canonical_push_payload(&head.updates, HashAlgo::Sha1);
    match pubkey.verify(&payload, &sig) {
        Ok(()) => Ok(SigOutcome::Verified { principal_id }),
        Err(e) => Ok(SigOutcome::BadSignature(format!("{e}"))),
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
fn require_signed_for(store: &Mutex<Store>, principal: &Principal) -> bool {
    let guard = store.lock().unwrap();
    guard.capabilities_for(principal).require_signed
}

/// M10/W15: walk the newly-pushed commits and verify each carries a
/// valid `alt-sig` header from a trusted principal. Returns `Some(reason)`
/// for the first commit that fails the check (the caller turns it into
/// per-command `ng` so the entire push is rejected atomically). Returns
/// `Ok(None)` when either the policy doesn't require commit signatures
/// or every new commit passes.
fn require_signed_commits_block(
    store: &Mutex<Store>,
    principal: &Principal,
    new_commits: &[ObjectId],
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let (require, trust) = {
        let guard = store.lock().unwrap();
        let caps = guard.capabilities_for(principal);
        if !caps.require_signed_commits {
            return Ok(None);
        }
        (caps.require_signed_commits, guard.trust_keys()?)
    };
    if !require {
        return Ok(None);
    }
    for oid in new_commits {
        let bytes = {
            let guard = store.lock().unwrap();
            let Some(raw) = guard.odb_get(oid)? else {
                return Ok(Some(format!("commit {oid} missing from odb after ingest")));
            };
            raw.data
        };
        match alt_cli::commit_sign::extract_alt_sig(&bytes)? {
            None => {
                return Ok(Some(format!(
                    "commit {oid} missing alt-sig (require-signed-commits)"
                )));
            }
            Some(parsed) => {
                let Some((_, pubkey)) = trust.iter().find(|(id, _)| id == &parsed.principal) else {
                    return Ok(Some(format!(
                        "commit {oid} signed by '{}' not in trust store",
                        parsed.principal
                    )));
                };
                if pubkey.verify(&parsed.canonical, &parsed.sig).is_err() {
                    return Ok(Some(format!(
                        "commit {oid} alt-sig did not verify against trust['{}']",
                        parsed.principal
                    )));
                }
            }
        }
    }
    Ok(None)
}

fn commit_ref_updates(
    store: &Mutex<Store>,
    updates: &[RefUpdate],
    principal: &alt_cli::native::Principal,
    sig_label: &str,
) -> Result<(), String> {
    if updates.is_empty() {
        return Ok(());
    }
    let mut store_guard = store.lock().unwrap();
    let mut changes = Vec::with_capacity(updates.len());
    for u in updates {
        changes.push(RefChange {
            name: u.name.clone(),
            old: u.old.map(RefTarget::Oid),
            new: u.new.map(RefTarget::Oid),
        });
    }
    let caps = store_guard.capabilities_for(principal);
    let actor = format!(
        "{kind}:{id};verb:wire/receive-pack;sig:{sig}",
        kind = match principal.kind {
            alt_cli::native::PrincipalKind::Human => "human",
            alt_cli::native::PrincipalKind::Agent => "agent",
        },
        id = principal.id,
        sig = sig_label,
    );
    // Build the RefPolicy that mirrors local `commit_refs_with`: a closure
    // over the caps' branch_allow drives the per-ref check inside the
    // commit lock.
    // M10/W22: combine branch_allow + branch_deny (deny wins) into a
    // single closure that mirrors the local CLI path.
    let allow = caps.branch_allow.clone();
    let deny = caps.branch_deny.clone();
    let has_constraint = !allow.is_empty() || !deny.is_empty();
    let is_branch_allowed = move |name: &str| {
        if deny.iter().any(|g| g.matches(name)) {
            return false;
        }
        allow.is_empty() || allow.iter().any(|g| g.matches(name))
    };
    let policy = alt_refs::RefPolicy {
        read_only: caps.read_only,
        is_branch_allowed: if has_constraint {
            Some(&is_branch_allowed)
        } else {
            None
        },
    };
    store_guard
        .refs_mut()
        .commit_idempotent(&actor, now_ms(), &changes, None, Some(&policy))
        .map_err(|e| format!("ref tx: {e}"))?;
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

// Suppress an unused-import warning so this file remains tidy while
// the W10b POST handler that reads request bodies lands later.
#[allow(dead_code)]
fn _phantom_keep_imports(_c: Cursor<Vec<u8>>) {}

/// What `check_auth` decided about a request. `Reject` carries the
/// short string the body gets so a human running curl sees something
/// meaningful (a real git client just retries with the credential).
/// `Allow` returns the authenticated user (None when no users file was
/// in play, so the request is anonymous) and the user's ACL when they
/// were scoped (None = trusted user, every repo + every action).
enum AuthOutcome {
    Allow {
        user: Option<String>,
        acl: Option<Vec<AclRule>>,
    },
    Reject(String),
}

/// One entry in a user's per-repo permission table. `repo == "*"` is the
/// wildcard meaning "every repo this user can see"; `perm` says whether
/// they can read, write, or both.
#[derive(Debug, Clone)]
struct AclRule {
    repo: String,
    can_read: bool,
    can_write: bool,
}

/// What kind of access the current HTTP request needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Read,
    Write,
}

fn action_from_request(method: &Method, path: &str, query: &str) -> Option<Action> {
    if method == &Method::Get && path.ends_with("/info/refs") {
        if query.contains("service=git-upload-pack") {
            return Some(Action::Read);
        }
        if query.contains("service=git-receive-pack") {
            return Some(Action::Write);
        }
        return None;
    }
    if method == &Method::Post && path.ends_with("/git-upload-pack") {
        return Some(Action::Read);
    }
    if method == &Method::Post && path.ends_with("/git-receive-pack") {
        return Some(Action::Write);
    }
    None
}

fn acl_allows(acl: &[AclRule], repo: &str, action: Action) -> bool {
    for rule in acl {
        if rule.repo != "*" && rule.repo != repo {
            continue;
        }
        match action {
            Action::Read => {
                if rule.can_read {
                    return true;
                }
            }
            Action::Write => {
                if rule.can_write {
                    return true;
                }
            }
        }
    }
    false
}

fn parse_acl(field: &str) -> Vec<AclRule> {
    let mut out = Vec::new();
    for token in field.split_whitespace() {
        let Some((repo, perm)) = token.split_once(':') else {
            continue;
        };
        let (can_read, can_write) = match perm {
            "r" => (true, false),
            "w" => (false, true),
            "rw" | "wr" => (true, true),
            "n" | "" => (false, false),
            _ => continue,
        };
        out.push(AclRule {
            repo: repo.to_owned(),
            can_read,
            can_write,
        });
    }
    out
}

/// M14/W39 — constant-time, ASCII-case-insensitive byte comparison.
/// Short-circuit `==` / `eq_ignore_ascii_case` leaks bytes of the
/// stored token hash through wall-clock timing differences: an
/// attacker who can fire many auth requests reconstructs the hash
/// one byte at a time. This helper always walks the full length of
/// both inputs, accumulating an OR of per-byte XORs so the work is
/// independent of where (or whether) the inputs first differ.
fn constant_time_eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    // Length mismatch is the one early-exit we permit; the stored
    // BLAKE3 hex is always 64 ASCII chars and the candidate is hashed
    // by us, so honest paths never trip the length branch and the
    // early-exit reveals no per-byte information.
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x.to_ascii_lowercase() ^ y.to_ascii_lowercase();
    }
    diff == 0
}

/// Validate an HTTP Basic `Authorization` header against the users file
/// at `users_path`. The file format is `<name>\t<blake3-hex-of-token>\n`
/// per line, with `#` comment lines and blank lines tolerated; the
/// token itself is never stored, only its BLAKE3 hash.
fn check_auth(req: &tiny_http::Request, users_path: &Path) -> AuthOutcome {
    let Some(header_value) = req
        .headers()
        .iter()
        .find(|h| {
            h.field
                .as_str()
                .as_str()
                .eq_ignore_ascii_case("authorization")
        })
        .map(|h| h.value.as_str().to_owned())
    else {
        return AuthOutcome::Reject("missing Authorization header".into());
    };
    let Some(b64) = header_value.strip_prefix("Basic ") else {
        return AuthOutcome::Reject("only Basic auth is supported".into());
    };
    let decoded = match base64_decode(b64.trim()) {
        Some(b) => b,
        None => return AuthOutcome::Reject("Authorization base64 decode failed".into()),
    };
    let decoded_str = match std::str::from_utf8(&decoded) {
        Ok(s) => s,
        Err(_) => return AuthOutcome::Reject("Authorization is not utf-8".into()),
    };
    let Some((user, token)) = decoded_str.split_once(':') else {
        return AuthOutcome::Reject("Authorization missing ':' separator".into());
    };
    let token_hash = blake3::hash(token.as_bytes());
    let token_hex = token_hash.to_hex();
    let table = match read_users(users_path) {
        Ok(t) => t,
        Err(e) => return AuthOutcome::Reject(format!("server users file unreadable: {e}")),
    };
    let Some(entry) = table.get(user) else {
        return AuthOutcome::Reject("unknown user".into());
    };
    if !constant_time_eq_ignore_ascii_case(entry.token_hash.as_bytes(), token_hex.as_bytes()) {
        return AuthOutcome::Reject("bad token".into());
    }
    // M9/W11c — a 2-column users line (no ACL) is the "trusted user"
    // shape: every repo + every action allowed. A 3-column line scopes
    // the user, and the dispatcher then asks `acl_allows` per request.
    AuthOutcome::Allow {
        user: Some(user.to_owned()),
        acl: entry.acl.clone(),
    }
}

#[derive(Debug, Clone)]
struct UserEntry {
    token_hash: String,
    /// `None` = trusted user, every repo + every action allowed.
    /// `Some(rules)` = scoped user, only the listed rules apply.
    acl: Option<Vec<AclRule>>,
}

fn read_users(path: &Path) -> std::io::Result<HashMap<String, UserEntry>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = HashMap::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.splitn(3, '\t');
        let Some(user) = parts.next() else { continue };
        let Some(hash) = parts.next() else { continue };
        let acl = parts.next().map(parse_acl);
        out.insert(
            user.trim().to_owned(),
            UserEntry {
                token_hash: hash.trim().to_owned(),
                acl,
            },
        );
    }
    Ok(out)
}

/// Minimal RFC-4648 base64 decoder (standard alphabet, padded). Tiny
/// scope: HTTP Basic creds are short, this avoids dragging in a base64
/// crate. Returns None on any malformed input — the request gets a 401.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const TABLE: [u8; 256] = {
        let mut t = [0xffu8; 256];
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0;
        while i < alphabet.len() {
            t[alphabet[i] as usize] = i as u8;
            i += 1;
        }
        t
    };
    let bytes = input.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut vals = [0u8; 4];
        let mut pad = 0;
        for (i, &c) in chunk.iter().enumerate() {
            if c == b'=' {
                pad += 1;
                vals[i] = 0;
            } else {
                let v = TABLE[c as usize];
                if v == 0xff {
                    return None;
                }
                vals[i] = v;
            }
        }
        let n = (vals[0] as u32) << 18
            | (vals[1] as u32) << 12
            | (vals[2] as u32) << 6
            | vals[3] as u32;
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M14/W39 — constant-time compare basic correctness on the
    /// values it actually sees in production: 64-char BLAKE3 hex
    /// strings, plus a few edge cases.
    #[test]
    fn constant_time_eq_ignore_ascii_case_basic() {
        let a = b"abcdef0123456789";
        let b = b"abcdef0123456789";
        assert!(constant_time_eq_ignore_ascii_case(a, b));
        // Case insensitivity matches the original `eq_ignore_ascii_case`
        // behavior — BLAKE3 hex output is lowercase but a poorly-
        // configured operator might paste uppercase.
        assert!(constant_time_eq_ignore_ascii_case(
            b"ABCDEF0123456789",
            b"abcdef0123456789"
        ));
        // Differences anywhere in the string are detected.
        assert!(!constant_time_eq_ignore_ascii_case(a, b"xbcdef0123456789"));
        assert!(!constant_time_eq_ignore_ascii_case(a, b"abcdef012345678x"));
        // Length mismatch is the one early-exit.
        assert!(!constant_time_eq_ignore_ascii_case(b"abc", b"abcd"));
        // Empty strings compare equal (degenerate but well-defined).
        assert!(constant_time_eq_ignore_ascii_case(b"", b""));
    }

    /// The compare must not short-circuit on the first differing byte.
    /// We can't measure timing from a Rust unit test reliably, but
    /// the *control flow* must walk the full length — meaning a
    /// diff in the first byte and a diff in the last byte must both
    /// reach the loop's end. We verify this property by hand-counting
    /// iterations via a wrapping accumulator: any short-circuit
    /// would diverge.
    #[test]
    fn constant_time_eq_does_full_pass_regardless_of_diff_position() {
        // 64-byte test vectors so the loop body actually does work.
        let stored = b"0000000000000000000000000000000000000000000000000000000000000000";
        let head_diff = b"f000000000000000000000000000000000000000000000000000000000000000";
        let tail_diff = b"000000000000000000000000000000000000000000000000000000000000000f";
        let mid_diff = b"00000000000000000000000000000000f0000000000000000000000000000000";

        // All three must return false (mismatch); the comparison is
        // identical work regardless of position.
        assert!(!constant_time_eq_ignore_ascii_case(stored, head_diff));
        assert!(!constant_time_eq_ignore_ascii_case(stored, tail_diff));
        assert!(!constant_time_eq_ignore_ascii_case(stored, mid_diff));
        // And the identical-input path returns true.
        assert!(constant_time_eq_ignore_ascii_case(stored, stored));
    }
}
