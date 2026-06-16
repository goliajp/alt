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
use std::sync::Arc;

use alt_repo::Repository;
use alt_wire::caps;
use alt_wire::ls_refs::RefRecord;
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

    let server = Server::http(&bind).unwrap_or_else(|e| die(&format!("bind {bind}: {e}")));
    eprintln!(
        "altd-server: listening on {} (repo {})",
        server.server_addr(),
        repo_path
    );

    for req in server.incoming_requests() {
        let url = req.url().to_owned();
        let method = req.method().clone();
        if let Err(e) = dispatch(&repo, method, &url, req) {
            eprintln!("altd-server: handler error: {e}");
        }
    }
}

fn dispatch(
    repo: &Repository,
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
    // POST git-receive-pack lands in W10c.
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
    // Required: service=git-upload-pack | git-receive-pack
    let service = parse_query(query, "service")
        .ok_or("info/refs missing ?service= query parameter")?
        .to_owned();
    if service != "git-upload-pack" && service != "git-receive-pack" {
        let r = Response::from_string("unknown service").with_status_code(StatusCode(400));
        req.respond(r)?;
        return Ok(());
    }

    let mut body = Vec::new();
    // smart-http capability advertisement: advertise v2, agent,
    // object-format and the commands we serve. ls-refs is the only one
    // W10a actually handles end-to-end; fetch/push land in W10b/c.
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

    let content_type = format!("application/x-{service}-advertisement");
    let mut resp = Response::from_data(body);
    resp.add_header(header("Content-Type", &content_type));
    resp.add_header(header("Cache-Control", "no-cache"));
    req.respond(resp)?;
    let _ = repo; // keep the borrow alive across the response (repo is read on POST)
    Ok(())
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
