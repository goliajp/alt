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
#[path = "altd_server/check.rs"]
mod check;
#[path = "altd_server/dispatch.rs"]
mod dispatch;
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
#[path = "altd_server/reached.rs"]
mod reached;
#[path = "altd_server/readers.rs"]
mod readers;
#[path = "altd_server/sending.rs"]
mod sending;
#[path = "altd_server/shallow.rs"]
mod shallow;
#[path = "altd_server/shutdown.rs"]
mod shutdown;
#[path = "altd_server/told.rs"]
mod told;
#[path = "altd_server/until.rs"]
mod until;
#[path = "altd_server/upload.rs"]
mod upload;
#[path = "altd_server/workers.rs"]
mod workers;

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
use readers::{Reading, RepoHandle};
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
                    "usage: altd-server [--bind 127.0.0.1:PORT]\n       \
                     altd-server check-users <users file>   the lines that give every repository, or less than when rules were added up\n\n\
                     env:\n  \
                     ALT_SERVER_REPO          single alt repo to serve (legacy mode)\n  \
                     ALT_SERVER_ROOT          multi-repo root; URLs map /<name>/… → <root>/<name>\n  \
                     ALT_SERVER_REQUIRE_AUTH  set to 1 to refuse start when <root>/users is absent (fail-open auth guard)\n{}",
                    told::HELP
                );
                return;
            }
            "check-users" => match args.get(i + 1) {
                Some(path) => check::run(path),
                None => die("check-users needs the users file"),
            },
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

    // N workers each take requests in a loop, so that concurrent
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
    shutdown::install_signal_handlers();
    let most = (told.connections, workers / 2);
    let server = Server::listen(&bind, told.io, most, told.here, told.proxies);
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

    let workers = workers::Workers::start(Arc::clone(&server), Arc::new(mode), workers);

    // Poll the shutdown flag on the main thread. Once SIGTERM/SIGINT
    // fires, the workers take no request that is not there yet (in-flight
    // responses keep going) and are waited for. The poll is cheap
    // (100 ms) and only the main thread runs it.
    while !shutdown::SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    eprintln!("altd-server: shutdown signal received, unblocking workers");

    // a stuck worker (long ingest_pack, a malicious slow
    // client, a deadlocked Mutex<Store>) must not pin the process
    // forever. Spawn a watchdog that hard-exits after the deadline if
    // the workers have not ended by then. systemd / k8s see
    // exit 0 either way — graceful was the goal, the deadline is the
    // hard limit, both paths are an acceptable shutdown.
    let deadline_ms = told.shutdown.as_millis();
    let _watchdog = std::thread::spawn(move || {
        std::thread::sleep(told.shutdown);
        eprintln!("altd-server: graceful shutdown timed out after {deadline_ms} ms; force-exiting");
        std::process::exit(0);
    });
    workers.close();
    eprintln!("altd-server: all workers stopped, exiting cleanly");
}

fn die(msg: &str) -> ! {
    eprintln!("altd-server: {msg}");
    std::process::exit(2);
}
