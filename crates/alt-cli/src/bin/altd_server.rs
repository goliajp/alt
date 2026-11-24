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
#[path = "altd_server/log.rs"]
mod log;
#[path = "altd_server/mode.rs"]
mod mode;
#[path = "altd_server/nonces.rs"]
mod nonces;
#[path = "altd_server/push.rs"]
mod push;
#[path = "altd_server/readers.rs"]
mod readers;
#[path = "altd_server/sending.rs"]
mod sending;
#[path = "altd_server/shallow.rs"]
mod shallow;
#[path = "altd_server/told.rs"]
mod told;
#[path = "altd_server/upload.rs"]
mod upload;

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
use log::*;
use mode::*;
use nonces::*;
use push::*;
use readers::RepoHandle;
use upload::*;

const AGENT: &str = concat!("alt-server/", env!("CARGO_PKG_VERSION"));
const UPLOAD_PACK_RESULT: &str = "application/x-git-upload-pack-result";
const STOPPED: &str = "writes are stopped until the server is started again: a sync failed";

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
                     ALT_SERVER_REQUIRE_AUTH  set to 1 to refuse start when <root>/users is absent (fail-open auth guard)\n{}",
                    told::HELP
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
    let told = told::told();
    let workers = told.workers;

    // install signal handlers so SIGINT (Ctrl-C) and SIGTERM
    // (`systemctl stop`, `docker stop`, k8s preStop) tip the shutdown
    // flag. The handler must stay async-signal-safe, so it only does an
    // atomic store; the main thread polls it and drives the actual
    // shutdown sequence. Installed before the listening line: a
    // supervisor may signal as soon as it sees it, and a signal landing
    // earlier would take the default action and skip the drain.
    install_signal_handlers();

    let server = Server::listen(&bind, told.io, told.connections, told.here);
    let server = Arc::new(server.unwrap_or_else(|e| die(&format!("bind {bind}: {e}"))));
    eprintln!(
        "altd-server: listening on {} ({}, workers={workers})",
        server.server_addr(),
        mode.describe()
    );

    if told.origins.is_empty() {
        eprintln!(
            "altd-server: ALT_SERVER_ORIGINS is not set: a signed push is taken for the Host header of its request, also one that was signed for another server and is sent on under that server's name"
        );
    }

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
    let deadline_ms = told.shutdown.as_millis();
    let _watchdog = std::thread::spawn(move || {
        std::thread::sleep(told.shutdown);
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
    if log.deferred {
        return;
    }
    let duration_ms = start.elapsed().as_millis();
    emit_access_log(&req_id, &method, &url, &log, duration_ms);
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
        // a body is received by the thread of its connection, and the
        // request comes here again with it
        if method == Method::Post && !req.received() {
            log.deferred = true;
            req.receive(limits::body_cap(&suffix));
            return Ok(());
        }
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
            let answered = handle_receive_pack(&repo, &writer, auth_user.as_deref(), req, log);
            // a sync that failed is known of the repository from here on,
            // though this request be the last to have its writer in hand
            writer.remember();
            return answered;
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

fn die(msg: &str) -> ! {
    eprintln!("altd-server: {msg}");
    std::process::exit(2);
}
