//! Bind a `tiny_http::Server` to the API endpoints, dispatch one request
//! at a time. Sync, blocking, no async runtime — same shape as
//! [`altd-server`]. A handful of workers is enough for a marketing
//! domain.
//!
//! The router is intentionally tiny: there are three endpoints and one
//! catch-all fallback. Adding a new endpoint means adding a branch
//! below; the matcher does no regex or path-template work.

use std::sync::Arc;
use std::thread;

use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::api;
use crate::{ApiError, Source};

/// Mount the API on `bind` and serve it. Blocks the calling thread; a
/// SIGINT / SIGTERM landing on the process kills the [`Server`] handle
/// and the call returns.
///
/// `workers` is the number of dispatcher threads sharing the incoming
/// request queue.
pub fn serve(bind: &str, source: Source, workers: usize) -> std::io::Result<()> {
    let server = Server::http(bind).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("tiny_http bind {bind}: {e}"),
        )
    })?;
    eprintln!(
        "alt-web: listening on {bind} (workers={workers}, .alt={})",
        source.alt_dir().display()
    );
    let server = Arc::new(server);
    let source = Arc::new(source);

    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let server = Arc::clone(&server);
        let source = Arc::clone(&source);
        handles.push(thread::spawn(move || {
            while let Ok(req) = server.recv() {
                dispatch(&source, req);
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

/// Match the request against the four endpoints; everything else is a
/// 404. Each handler returns owned bytes + status; the dispatcher
/// attaches headers and the response body.
fn dispatch(source: &Source, req: Request) {
    if req.method() != &Method::Get {
        respond_json(req, 405, b"{\"error\":\"method not allowed\"}");
        return;
    }
    let (path, query) = match req.url().split_once('?') {
        Some((p, q)) => (p, q),
        None => (req.url(), ""),
    };

    let (status, body) = if path == "/api/version" {
        api::handle_version()
    } else if path == "/api/stats" {
        match api::handle_stats(source) {
            Ok(v) => v,
            Err(e) => error_response(&e),
        }
    } else if path == "/api/changelog" {
        let n = parse_n_query(query).unwrap_or(10);
        match api::handle_changelog(source, n) {
            Ok(v) => v,
            Err(e) => error_response(&e),
        }
    } else if path == "/" {
        (
            200,
            b"alt.golia.jp \xe2\x80\x94 pure-Rust VCS. /api/version, /api/stats, /api/changelog. Source: https://github.com/goliajp/alt\n".to_vec(),
        )
    } else {
        (404, b"{\"error\":\"not found\"}".to_vec())
    };

    respond_json(req, status, &body);
}

/// Read `?n=<usize>` out of the query string; clamps invalid input to
/// `None` so the caller falls back to a default.
fn parse_n_query(query: &str) -> Option<usize> {
    for pair in query.split('&') {
        if let Some(v) = pair.strip_prefix("n=")
            && let Ok(n) = v.parse::<usize>()
        {
            return Some(n);
        }
    }
    None
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
    let _ = req.respond(resp);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_n_query_picks_first_n() {
        assert_eq!(parse_n_query("n=5"), Some(5));
        assert_eq!(parse_n_query("foo=bar&n=12&baz=qux"), Some(12));
        assert_eq!(parse_n_query("foo=bar"), None);
        assert_eq!(parse_n_query(""), None);
        assert_eq!(parse_n_query("n=not-a-number"), None);
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
