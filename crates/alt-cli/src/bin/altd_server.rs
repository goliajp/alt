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
                    "usage: altd-server [--bind 127.0.0.1:PORT]\n\n\
                     env:\n  \
                     ALT_SERVER_REPO  path to a single alt repo to serve (legacy mode)\n  \
                     ALT_SERVER_ROOT  path to a multi-repo root; URLs map /<name>/… → <root>/<name>"
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

    let server = Server::http(&bind).unwrap_or_else(|e| die(&format!("bind {bind}: {e}")));
    eprintln!(
        "altd-server: listening on {} ({})",
        server.server_addr(),
        mode.describe()
    );

    for req in server.incoming_requests() {
        let url = req.url().to_owned();
        let method = req.method().clone();
        if let Err(e) = dispatch(&mode, method, &url, req) {
            eprintln!("altd-server: handler error: {e}");
        }
    }
}

/// Single-repo (ALT_SERVER_REPO) or multi-repo (ALT_SERVER_ROOT) serve.
/// Single keeps W10's old shape — info/refs lives at the URL root.
/// Multi parses the first path segment as a repo name and resolves it
/// under the configured root.
enum ServeMode {
    Single {
        repo_path: PathBuf,
        repo: Arc<Repository>,
        store: Option<Arc<Mutex<Store>>>,
    },
    Multi {
        root: PathBuf,
        cache: Mutex<HashMap<String, RepoHandle>>,
    },
}

struct RepoHandle {
    repo: Arc<Repository>,
    store: Option<Arc<Mutex<Store>>>,
}

impl ServeMode {
    fn single(repo_path: PathBuf) -> Self {
        let repo = Arc::new(
            Repository::discover(&repo_path).unwrap_or_else(|e| die(&format!("opening repo: {e}"))),
        );
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
        ServeMode::Single {
            repo_path,
            repo,
            store,
        }
    }

    fn multi(root: PathBuf) -> Self {
        if !root.is_dir() {
            die(&format!(
                "ALT_SERVER_ROOT={} does not exist or isn't a directory",
                root.display()
            ));
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
        ServeMode::Single { repo, store, .. } => Ok((
            RepoHandle {
                repo: repo.clone(),
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
                        repo: h.repo.clone(),
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
            let repo = Arc::new(Repository::discover(&repo_path)?);
            let alt_dir = repo_path.join(".alt");
            let store: Option<Arc<Mutex<Store>>> = if alt_dir.is_dir() {
                Some(Arc::new(Mutex::new(Store::open(alt_dir)?)))
            } else {
                None
            };
            let handle = RepoHandle {
                repo: repo.clone(),
                store: store.clone(),
            };
            cache_g.insert(name.to_owned(), handle);
            Ok((RepoHandle { repo, store }, rest.to_owned(), name.to_owned()))
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
) -> Result<(), Box<dyn std::error::Error>> {
    // M9/W11b — optional Basic auth in multi-repo mode. When the server
    // root has a `users` file, every request must carry a valid
    // Authorization header; absence / mismatch returns HTTP 401 with a
    // WWW-Authenticate prompt so a real git client retries with creds.
    // M9/W11c — a scoped user (3-column line) hands back an ACL the
    // dispatcher checks against the resolved repo + action below.
    let mut scoped_acl: Option<Vec<AclRule>> = None;
    if let ServeMode::Multi { root, .. } = mode {
        let users_path = root.join("users");
        if users_path.is_file() {
            match check_auth(&req, &users_path) {
                AuthOutcome::Ok => {}
                AuthOutcome::OkScoped { acl, .. } => scoped_acl = Some(acl),
                AuthOutcome::Reject(reason) => {
                    let mut resp = Response::from_string(reason).with_status_code(StatusCode(401));
                    resp.add_header(header("WWW-Authenticate", "Basic realm=\"altd-server\""));
                    req.respond(resp)?;
                    return Ok(());
                }
            }
        }
    }

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
            req.respond(r)?;
            return Ok(());
        }
    };

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
        req.respond(r)?;
        return Ok(());
    }

    if method == Method::Get && suffix.ends_with("/info/refs") {
        return handle_info_refs(&handle.repo, query, req);
    }
    if method == Method::Post && suffix.ends_with("/git-upload-pack") {
        return handle_upload_pack(&handle.repo, req);
    }
    if method == Method::Post && suffix.ends_with("/git-receive-pack") {
        let Some(store) = handle.store else {
            let r = Response::from_string("repo is read-only (no .alt write store)")
                .with_status_code(StatusCode(403));
            req.respond(r)?;
            return Ok(());
        };
        return handle_receive_pack(&handle.repo, &store, req);
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

/// What `check_auth` decided about a request. `Reject` carries the
/// short string the body gets so a human running curl sees something
/// meaningful (a real git client just retries with the credential).
/// `OkScoped` means authentication succeeded *and* the user has an ACL
/// the dispatcher then checks against the resolved repo + action.
enum AuthOutcome {
    Ok,
    OkScoped {
        #[allow(dead_code)]
        user: String,
        acl: Vec<AclRule>,
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
    if !entry.token_hash.eq_ignore_ascii_case(token_hex.as_str()) {
        return AuthOutcome::Reject("bad token".into());
    }
    // M9/W11c — a 2-column users line (no ACL) is the "trusted user"
    // shape: every repo + every action allowed. A 3-column line scopes
    // the user, and the dispatcher then asks `acl_allows` per request.
    match &entry.acl {
        None => AuthOutcome::Ok,
        Some(rules) => AuthOutcome::OkScoped {
            user: user.to_owned(),
            acl: rules.clone(),
        },
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
