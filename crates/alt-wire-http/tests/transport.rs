//! Hermetic round-trip tests for [`GitTransport`]: bring up a TCP listener,
//! play one HTTP request, assert what the server saw and what the client
//! got back. No network, no TLS — plain HTTP/1.1 on a loopback port.
//!
//! This isn't a full HTTP server; we parse exactly the shape ureq sends
//! (CRLF-delimited headers, optional Content-Length body) and we write
//! exactly the shape git servers do. If a future ureq release reframes
//! requests, these tests will fire — which is what we want, because the
//! transport contract is what the wire actually carries.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use alt_wire_http::{BasicAuth, GitTransport, Service, TransportError};

/// Everything the mock server captured from one request.
#[derive(Debug, Clone)]
struct Captured {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Spawn a one-shot HTTP server on a random loopback port. Reads one
/// request, captures it, replies with `response_body` (Content-Type set
/// from the request's Accept header for fidelity to git servers).
///
/// Returns the `(url_base, captured_receiver)`: the URL the client should
/// dial, and a channel that yields the parsed request when the server is
/// done.
fn spawn_one_shot_server(response_body: Vec<u8>) -> (String, mpsc::Receiver<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let captured = read_request(&mut stream);
        // echo the Accept value as the response Content-Type so the client
        // sees a sensible content-type round-trip
        let ct = captured
            .header("Accept")
            .filter(|s| !s.contains(','))
            .unwrap_or("application/octet-stream")
            .to_owned();
        write_response(&mut stream, 200, &ct, &response_body);
        let _ = tx.send(captured);
    });
    (format!("http://127.0.0.1:{port}"), rx)
}

fn read_request(stream: &mut TcpStream) -> Captured {
    // pull bytes until we have the headers (\r\n\r\n)
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end;
    loop {
        let n = stream.read(&mut tmp).expect("read");
        if n == 0 {
            panic!("client closed before sending headers");
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = find_subseq(&buf, b"\r\n\r\n") {
            header_end = p + 4;
            break;
        }
    }
    let header_bytes = &buf[..header_end - 4];
    let header_text = std::str::from_utf8(header_bytes).expect("utf-8 headers");
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().expect("request line");
    let mut parts = request_line.splitn(3, ' ');
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once(':').expect("header K: V");
        let v = v.trim().to_owned();
        if k.eq_ignore_ascii_case("content-length") {
            content_length = v.parse().unwrap_or(0);
        }
        headers.push((k.to_owned(), v));
    }
    // pull body if we've already read some of it
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut tmp).expect("read body");
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);
    Captured {
        method,
        path,
        headers,
        body,
    }
}

fn write_response(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n",
        len = body.len(),
    );
    stream.write_all(head.as_bytes()).expect("write head");
    stream.write_all(body).expect("write body");
}

fn find_subseq(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `info_refs` sends `GET /<repo>/info/refs?service=git-upload-pack` with
/// `Git-Protocol: version=2` and no auth header — the canonical
/// public-fetch shape.
#[test]
fn info_refs_sends_canonical_v2_get() {
    let server_body = b"001e# service=git-upload-pack\n00000016version 2\n0000".to_vec();
    let (base, rx) = spawn_one_shot_server(server_body.clone());

    let t = GitTransport::new(format!("{base}/repo.git"));
    let got = t.info_refs(Service::UploadPack).expect("info_refs");
    assert_eq!(got, server_body, "client received the server body verbatim");

    let req = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("server saw a request");
    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/repo.git/info/refs?service=git-upload-pack");
    assert_eq!(req.header("Git-Protocol"), Some("version=2"));
    assert!(
        req.header("Authorization").is_none(),
        "no auth header without credentials"
    );
    assert!(req.body.is_empty(), "GET has no body");
}

/// `command` sends `POST /<repo>/git-upload-pack` with the service's
/// Content-Type / Accept headers and the body verbatim. The response
/// body comes back unmodified.
#[test]
fn command_sends_canonical_v2_post_with_correct_headers() {
    let server_body = b"0008resp".to_vec();
    let (base, rx) = spawn_one_shot_server(server_body.clone());

    let t = GitTransport::new(format!("{base}/repo.git"));
    let req_body = b"0014command=ls-refs\n00010000".to_vec();
    let got = t.command(Service::UploadPack, &req_body).expect("command");
    assert_eq!(got, server_body, "client received the server body verbatim");

    let req = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("server saw a request");
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/repo.git/git-upload-pack");
    assert_eq!(req.header("Git-Protocol"), Some("version=2"));
    assert_eq!(
        req.header("Content-Type"),
        Some("application/x-git-upload-pack-request"),
    );
    assert_eq!(
        req.header("Accept"),
        Some("application/x-git-upload-pack-result"),
    );
    assert_eq!(req.body, req_body, "POST body passed through verbatim");
}

/// `with_auth` adds the `Authorization: Basic …` header on every request.
/// The encoding matches the inline base64 helper.
#[test]
fn with_auth_attaches_basic_authorization_header() {
    let (base, rx) = spawn_one_shot_server(b"0000".to_vec());
    let t = GitTransport::new(format!("{base}/repo.git")).with_auth(BasicAuth {
        username: "alice".into(),
        token: "ghp_secret".into(),
    });
    let _ = t.info_refs(Service::UploadPack).expect("info_refs");
    let req = rx.recv_timeout(Duration::from_secs(5)).expect("request");
    // base64("alice:ghp_secret") = "YWxpY2U6Z2hwX3NlY3JldA=="
    assert_eq!(
        req.header("Authorization"),
        Some("Basic YWxpY2U6Z2hwX3NlY3JldA==")
    );
}

/// `ReceivePack` flips the path suffix and the media types — push uses
/// its own pair.
#[test]
fn receive_pack_uses_push_endpoint_and_media_types() {
    let (base, rx) = spawn_one_shot_server(b"".to_vec());
    let t = GitTransport::new(format!("{base}/repo.git"));
    let _ = t.command(Service::ReceivePack, b"body").expect("command");
    let req = rx.recv_timeout(Duration::from_secs(5)).expect("request");
    assert_eq!(req.path, "/repo.git/git-receive-pack");
    assert_eq!(
        req.header("Content-Type"),
        Some("application/x-git-receive-pack-request"),
    );
    assert_eq!(
        req.header("Accept"),
        Some("application/x-git-receive-pack-result"),
    );
}

/// A non-2xx HTTP status surfaces as a typed [`TransportError::HttpStatus`]
/// with the server's body captured — git servers send useful stderr-style
/// text in 401/404/500 bodies, agents need that to act.
#[test]
fn http_error_status_surfaces_typed_with_body() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        write_response(&mut stream, 401, "text/plain", b"please log in\n");
    });
    let t = GitTransport::new(format!("http://127.0.0.1:{port}/repo.git"));
    let err = t.info_refs(Service::UploadPack).expect_err("should fail");
    match err {
        TransportError::HttpStatus { status, body } => {
            assert_eq!(status, 401);
            assert!(body.contains("please log in"), "got body: {body:?}");
        }
        other => panic!("expected HttpStatus, got {other:?}"),
    }
}
