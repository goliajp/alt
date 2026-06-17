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

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use alt_cli::native::Store;
use alt_git_codec::{HashAlgo, ObjectId};
use alt_refs::{RefChange, RefTarget};
use alt_repo::Repository;
use alt_wire::caps;
use alt_wire::ls_refs::RefRecord;
use alt_wire::push::{CommandStatus, RefUpdate};
use tiny_http::{Header, Method, Response, Server, StatusCode};

const AGENT: &str = concat!("alt-server/", env!("CARGO_PKG_VERSION"));

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
                    "usage: altd-server [--bind 127.0.0.1:PORT]\n\nenv:\n  ALT_SERVER_REPO  path to the alt repo to serve"
                );
                return;
            }
            other => die(&format!("unknown arg {other:?}")),
        }
        i += 1;
    }
    let repo_path = std::env::var("ALT_SERVER_REPO")
        .unwrap_or_else(|_| die("ALT_SERVER_REPO not set; point it at the repo to serve"));

    let repo = Arc::new(
        Repository::discover(&PathBuf::from(&repo_path))
            .unwrap_or_else(|e| die(&format!("opening repo: {e}"))),
    );
    // The write store: receive-pack mutates this (odb + refs). Reads
    // (ls-refs, fetch) still go through Repository so they don't fight
    // for the write lock. Both opens are on the same .alt dir; alt-odb's
    // flock serialises writers safely.
    let alt_dir = PathBuf::from(&repo_path).join(".alt");
    let store: Option<Arc<Mutex<Store>>> = if alt_dir.is_dir() {
        Some(Arc::new(Mutex::new(Store::open(alt_dir).unwrap_or_else(
            |e| die(&format!("opening write store: {e}")),
        ))))
    } else {
        eprintln!(
            "altd-server: no .alt under {repo_path}; receive-pack will be refused (read-only mode)"
        );
        None
    };

    let server = Server::http(&bind).unwrap_or_else(|e| die(&format!("bind {bind}: {e}")));
    eprintln!(
        "altd-server: listening on {} (repo {})",
        server.server_addr(),
        repo_path
    );

    for req in server.incoming_requests() {
        let url = req.url().to_owned();
        let method = req.method().clone();
        if let Err(e) = dispatch(&repo, store.as_deref(), method, &url, req) {
            eprintln!("altd-server: handler error: {e}");
        }
    }
}

fn dispatch(
    repo: &Repository,
    store: Option<&Mutex<Store>>,
    method: Method,
    url: &str,
    req: tiny_http::Request,
) -> Result<(), Box<dyn std::error::Error>> {
    // Path + query split. tiny_http hands us the raw `?…` tail.
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url, ""),
    };
    if method == Method::Get && path.ends_with("/info/refs") {
        return handle_info_refs(repo, query, req);
    }
    if method == Method::Post && path.ends_with("/git-upload-pack") {
        return handle_upload_pack(repo, req);
    }
    if method == Method::Post && path.ends_with("/git-receive-pack") {
        let Some(store) = store else {
            let r = Response::from_string("repo is read-only (no .alt write store)")
                .with_status_code(StatusCode(403));
            req.respond(r)?;
            return Ok(());
        };
        return handle_receive_pack(repo, store, req);
    }
    let r = Response::from_string("not found").with_status_code(StatusCode(404));
    req.respond(r)?;
    Ok(())
}

/// POST /git-upload-pack. v2 dispatch on the first `command=…` line:
/// `ls-refs` (W10a) and `fetch` (W10b) both land here. The request body
/// header is identical (`command=<x>\n` + optional `object-format=…`),
/// only the args section differs — sniff the first frame to route.
fn handle_upload_pack(
    repo: &Repository,
    mut req: tiny_http::Request,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut body = Vec::new();
    std::io::copy(req.as_reader(), &mut body)?;
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
            req.respond(r)?;
            return Ok(());
        }
    }

    let mut resp = Response::from_data(out);
    resp.add_header(header(
        "Content-Type",
        "application/x-git-upload-pack-result",
    ));
    resp.add_header(header("Cache-Control", "no-cache"));
    req.respond(resp)?;
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
    let dir = tempfile::tempdir()?;
    let count = u32::try_from(outgoing.len())
        .map_err(|_| "outgoing object set exceeds u32 (server-side)")?;
    let mut writer = alt_git_pack::PackWriter::create(dir.path(), repo.algo(), count)?;
    for (oid, kind) in &outgoing {
        let obj = repo
            .read_object(oid)?
            .ok_or_else(|| format!("outgoing object {oid} missing from server odb"))?;
        writer.add(*oid, *kind, &obj.data)?;
    }
    let written = writer.finish()?;
    Ok(std::fs::read(&written.pack_path)?)
}

fn handle_info_refs(
    repo: &Repository,
    query: &str,
    req: tiny_http::Request,
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
                    ("fetch", Some("shallow wait-for-done")),
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
            req.respond(r)?;
            return Ok(());
        }
    };
    let content_type = format!("application/x-{service}-advertisement");
    let mut resp = Response::from_data(body);
    resp.add_header(header("Content-Type", &content_type));
    resp.add_header(header("Cache-Control", "no-cache"));
    req.respond(resp)?;
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
    mut req: tiny_http::Request,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut body = Vec::new();
    std::io::copy(req.as_reader(), &mut body)?;
    let pushed = alt_wire::push::parse_push_request(&mut Cursor::new(&body), HashAlgo::Sha1)?;

    // Unpack the trailing pack into the store's odb. Empty pack = a
    // pure-delete push, no objects to index.
    let unpack_result: Result<(), String> = if pushed.pack.is_empty() {
        Ok(())
    } else {
        match ingest_pack(store, &pushed.pack) {
            Ok(()) => Ok(()),
            Err(e) => Err(format!("index-pack: {e}")),
        }
    };

    // Apply ref changes only if the pack unpacked cleanly; otherwise
    // mark every command `ng` so the client sees a coherent reason.
    let mut command_status: Vec<CommandStatus> = Vec::new();
    if unpack_result.is_ok() {
        match commit_ref_updates(store, &pushed.updates) {
            Ok(()) => {
                for u in &pushed.updates {
                    command_status.push(CommandStatus::Ok(u.name.clone()));
                }
            }
            Err(reason) => {
                for u in &pushed.updates {
                    command_status.push(CommandStatus::Ng {
                        name: u.name.clone(),
                        reason: reason.clone(),
                    });
                }
            }
        }
    } else {
        for u in &pushed.updates {
            command_status.push(CommandStatus::Ng {
                name: u.name.clone(),
                reason: "pack unpack failed".into(),
            });
        }
    }

    let mut out = Vec::new();
    alt_wire::push::encode_report_status(
        &mut out,
        unpack_result.as_ref().map(|_| ()).map_err(|s| s.as_str()),
        &command_status,
    )?;
    let mut resp = Response::from_data(out);
    resp.add_header(header(
        "Content-Type",
        "application/x-git-receive-pack-result",
    ));
    resp.add_header(header("Cache-Control", "no-cache"));
    req.respond(resp)?;
    let _ = repo; // borrow keepalive across response
    Ok(())
}

/// Write the pushed pack into a tempfile, index it, and put every
/// object into the server odb. Mirrors the fetch ingest path in
/// `alt-cli::native`.
fn ingest_pack(store: &Mutex<Store>, pack: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let tmp_pack = dir.path().join("incoming.pack");
    std::fs::write(&tmp_pack, pack)?;
    let indexed = alt_git_pack::index_pack(&tmp_pack, HashAlgo::Sha1, true)?;
    let ip = alt_git_pack::IndexedPack::open(&indexed.pack_path, HashAlgo::Sha1)?;
    let idx = ip.idx();
    let mut order: Vec<(u64, u32)> = (0..idx.len())
        .map(|i| (idx.offset_at(i).expect("idx in range"), i))
        .collect();
    order.sort_unstable();
    let mut store_guard = store.lock().unwrap();
    for (offset, i) in order {
        let obj = ip.read_at(offset)?;
        let _: ObjectId = idx.oid_at(i);
        let oid = idx.oid_at(i);
        store_guard.odb_mut().put(oid, obj.kind, &obj.data)?;
    }
    store_guard.odb_mut().flush()?;
    Ok(())
}

/// Apply the client's ref updates as a single ref transaction so the
/// server records the push as one op-log entry — same atomicity story
/// as a local `alt commit`.
fn commit_ref_updates(store: &Mutex<Store>, updates: &[RefUpdate]) -> Result<(), String> {
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
    let actor = "wire/receive-pack@altd-server";
    store_guard
        .refs_mut()
        .commit(actor, now_ms(), &changes)
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
