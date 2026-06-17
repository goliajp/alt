//! Bind a `tiny_http::Server` to the API endpoints, dispatch one request
//! at a time. Sync, blocking, no async runtime — same shape as
//! [`altd-server`]. A handful of workers is enough for a marketing
//! domain.
//!
//! Path matching is hand-written: small, regex-free, no path-template
//! work. Endpoints are organised into a route table inside [`dispatch`].

use std::sync::Arc;
use std::thread;

use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::api;
use crate::{ApiError, MultiRepo};

/// Mount the API on `bind` and serve it. Blocks the calling thread; a
/// SIGINT / SIGTERM landing on the process kills the [`Server`] handle
/// and the call returns.
pub fn serve(bind: &str, mr: MultiRepo, workers: usize) -> std::io::Result<()> {
    let server = Server::http(bind).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("tiny_http bind {bind}: {e}"),
        )
    })?;
    eprintln!(
        "alt-web: listening on {bind} (workers={workers}, root={})",
        mr.root().display()
    );
    let server = Arc::new(server);
    let mr = Arc::new(mr);

    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let server = Arc::clone(&server);
        let mr = Arc::clone(&mr);
        handles.push(thread::spawn(move || {
            while let Ok(req) = server.recv() {
                dispatch(&mr, req);
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

/// One handled request's response shape. Most endpoints return JSON;
/// `/api/repos/{name}/blob/{oid}/raw` returns binary with a sniffed
/// MIME so the frontend can embed images.
enum RouteResp {
    Json {
        status: u16,
        body: Vec<u8>,
    },
    Raw {
        status: u16,
        body: Vec<u8>,
        mime: &'static str,
    },
}

impl From<(u16, Vec<u8>)> for RouteResp {
    fn from((status, body): (u16, Vec<u8>)) -> Self {
        RouteResp::Json { status, body }
    }
}

fn dispatch(mr: &MultiRepo, req: Request) {
    if req.method() != &Method::Get {
        respond_json(req, 405, b"{\"error\":\"method not allowed\"}");
        return;
    }
    let (path, query) = match req.url().split_once('?') {
        Some((p, q)) => (p, q),
        None => (req.url(), ""),
    };

    match route(mr, path, query) {
        RouteResp::Json { status, body } => respond_json(req, status, &body),
        RouteResp::Raw { status, body, mime } => respond_raw(req, status, body, mime),
    }
}

fn route(mr: &MultiRepo, path: &str, query: &str) -> RouteResp {
    // /api/version (no repo)
    if path == "/api/version" {
        return api::handle_version().into();
    }
    // /api/repos
    if path == "/api/repos" {
        return collapse(api::handle_repos(mr)).into();
    }

    // /api/repos/{name}[/...]
    if let Some(rest) = path.strip_prefix("/api/repos/") {
        let (name, tail) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        return route_repo(mr, name, tail, query);
    }

    if path == "/" {
        return (
            200,
            b"alt.golia.jp \xe2\x80\x94 pure-Rust VCS. API at /api/. Source: https://github.com/goliajp/alt\n"
                .to_vec(),
        )
            .into();
    }
    (404u16, b"{\"error\":\"not found\"}".to_vec()).into()
}

fn route_repo(mr: &MultiRepo, name: &str, tail: &str, query: &str) -> RouteResp {
    if tail.is_empty() {
        return collapse(api::handle_repo(mr, name)).into();
    }
    if tail == "refs" {
        return collapse(api::handle_refs(mr, name)).into();
    }
    if tail == "log" {
        let n = parse_query(query, "n")
            .and_then(|v| v.parse().ok())
            .unwrap_or(20);
        let ref_name = parse_query(query, "ref");
        let before = parse_query(query, "before");
        return collapse(api::handle_log(
            mr,
            name,
            ref_name.as_deref(),
            n,
            before.as_deref(),
        ))
        .into();
    }
    if tail == "file_history" {
        let n = parse_query(query, "n")
            .and_then(|v| v.parse().ok())
            .unwrap_or(50);
        let ref_name = parse_query(query, "ref");
        let path = parse_query(query, "path").unwrap_or_default();
        return collapse(api::handle_file_history(
            mr,
            name,
            ref_name.as_deref(),
            &path,
            n,
        ))
        .into();
    }

    // /api/repos/{name}/commits/{oid}[/diff]
    if let Some(rest) = tail.strip_prefix("commits/") {
        let (oid, action) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        if action.is_empty() {
            return collapse(api::handle_commit(mr, name, oid)).into();
        }
        if action == "diff" {
            return collapse(api::handle_commit_diff(mr, name, oid)).into();
        }
    }

    // /api/repos/{name}/tree/{spec}
    if let Some(spec) = tail.strip_prefix("tree/") {
        return collapse(api::handle_tree(mr, name, spec)).into();
    }

    // /api/repos/{name}/blob/{oid}[/raw]
    if let Some(rest) = tail.strip_prefix("blob/") {
        let (oid, action) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        if action.is_empty() {
            return collapse(api::handle_blob(mr, name, oid)).into();
        }
        if action == "raw" {
            return match api::handle_blob_raw(mr, name, oid) {
                Ok((status, body, mime)) => RouteResp::Raw { status, body, mime },
                Err(e) => error_response(&e).into(),
            };
        }
    }
    (404u16, b"{\"error\":\"not found\"}".to_vec()).into()
}

fn parse_query(query: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=");
    for pair in query.split('&') {
        if let Some(v) = pair.strip_prefix(&needle) {
            return Some(url_decode(v));
        }
    }
    None
}

/// Minimal URL decoder — handles `%xx` hex and `+` → space, enough for
/// the ref names + oids we accept in query strings.
fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'+' {
            out.push(' ');
            i += 1;
        } else if b == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v as char);
                i += 3;
            } else {
                out.push(b as char);
                i += 1;
            }
        } else {
            out.push(b as char);
            i += 1;
        }
    }
    out
}

fn collapse(result: Result<(u16, Vec<u8>), ApiError>) -> (u16, Vec<u8>) {
    match result {
        Ok(v) => v,
        Err(e) => error_response(&e),
    }
}

fn error_response(err: &ApiError) -> (u16, Vec<u8>) {
    let body = format!(
        "{{\"schema_version\":1,\"error\":{{\"kind\":{},\"message\":{}}}}}",
        api::json_string(err.kind()),
        api::json_string(err.message()),
    );
    (err.status_code(), body.into_bytes())
}

fn respond_json(req: Request, status: u16, body: &[u8]) {
    let mut resp = Response::from_data(body.to_vec()).with_status_code(StatusCode(status));
    if let Ok(h) = Header::from_bytes(
        &b"Content-Type"[..],
        &b"application/json; charset=utf-8"[..],
    ) {
        resp.add_header(h);
    }
    if let Ok(h) = Header::from_bytes(&b"Cache-Control"[..], &b"no-cache"[..]) {
        resp.add_header(h);
    }
    // CORS: read-only API, no auth, public domain — let any origin hit it.
    if let Ok(h) = Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]) {
        resp.add_header(h);
    }
    let _ = req.respond(resp);
}

fn respond_raw(req: Request, status: u16, body: Vec<u8>, mime: &'static str) {
    let mut resp = Response::from_data(body).with_status_code(StatusCode(status));
    if let Ok(h) = Header::from_bytes(b"Content-Type", mime.as_bytes()) {
        resp.add_header(h);
    }
    // Raw blobs are content-addressed (oid in URL) — cache aggressively.
    if let Ok(h) = Header::from_bytes(
        &b"Cache-Control"[..],
        &b"public, max-age=31536000, immutable"[..],
    ) {
        resp.add_header(h);
    }
    if let Ok(h) = Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]) {
        resp.add_header(h);
    }
    let _ = req.respond(resp);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_picks_first_match() {
        assert_eq!(parse_query("n=5", "n"), Some("5".to_string()));
        assert_eq!(parse_query("foo=bar&n=12", "n"), Some("12".to_string()));
        assert_eq!(parse_query("foo=bar", "n"), None);
        assert_eq!(parse_query("", "n"), None);
    }

    #[test]
    fn url_decode_handles_hex_and_plus() {
        assert_eq!(url_decode("a+b"), "a b");
        assert_eq!(url_decode("a%20b"), "a b");
        assert_eq!(url_decode("refs%2Fheads%2Fmain"), "refs/heads/main");
        assert_eq!(url_decode("no-encoding"), "no-encoding");
    }

    #[test]
    fn error_response_shapes_status_and_body() {
        let err = ApiError::RepoOpen("missing .alt".into());
        let (status, body) = error_response(&err);
        assert_eq!(status, 503);
        let s = String::from_utf8(body).unwrap();
        assert!(s.contains("\"kind\":\"repo_unavailable\""), "{s}");
        assert!(s.contains("\"message\":\"missing .alt\""), "{s}");
    }
}
