//! HTTPS transport for git smart-http protocol v2 — the I/O sibling of
//! [`alt-wire`]. This crate's only job is to push pkt-line bytes over an
//! HTTP connection: encode the URL, set the right `Git-Protocol` /
//! `Content-Type` / `Accept` headers, handle Basic auth, surface server
//! errors typed.
//!
//! ## Why a separate crate
//!
//! `ureq` (with `rustls`) is alt's first HTTP/TLS dependency. Keeping it
//! out of [`alt-wire`] preserves that crate as pure logic (fuzzable
//! without booting a runtime) and gives downstreams a no-network path —
//! the protocol parser is useful in tests / replays without any TLS in the
//! build (same nerve as alt-treediff isolating `syn`).
//!
//! ## Surface
//!
//! - [`GitTransport`] — a handle to one remote:
//!   - [`info_refs`] — `GET <base>/info/refs?service=<svc>` (the capability
//!     advertisement; pipe the bytes through
//!     [`alt_wire::parse_capability_advertisement`]).
//!   - [`command`] — `POST <base>/<svc>` with the encoded command body and
//!     the protocol v2 headers. Returns the response body verbatim for the
//!     pkt-line parsers in [`alt-wire`]; `command_stream` hands it over as
//!     it comes, for a pack.
//! - [`BasicAuth`] — username + token for HTTPS auth (GitHub-style).
//! - [`Service`] — `UploadPack` for fetch / clone reads,
//!   `ReceivePack` for push writes.
//!
//! ## Scope
//!
//! Synchronous, blocking, one request at a time — same model as the rest
//! of alt (no async runtime). HTTP/2 is off (protocol v2 stateless POSTs
//! don't benefit), and no connection is kept for a next request. No credential helper
//! protocol — auth comes from caller-supplied env / config. SSH transport
//! is a later step.

use std::io::{self, Read};
use std::time::Duration;

/// The two git smart-http services. `UploadPack` reads (fetch / clone),
/// `ReceivePack` writes (push). Each carries its own URL path suffix and
/// `Content-Type` / `Accept` media types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    UploadPack,
    ReceivePack,
}

impl Service {
    /// The query-string value for `?service=…` on `info/refs`, and the
    /// trailing path segment on the command POST.
    pub fn name(self) -> &'static str {
        match self {
            Service::UploadPack => "git-upload-pack",
            Service::ReceivePack => "git-receive-pack",
        }
    }
    fn content_type(self) -> &'static str {
        match self {
            Service::UploadPack => "application/x-git-upload-pack-request",
            Service::ReceivePack => "application/x-git-receive-pack-request",
        }
    }
    fn accept(self) -> &'static str {
        match self {
            Service::UploadPack => "application/x-git-upload-pack-result",
            Service::ReceivePack => "application/x-git-receive-pack-result",
        }
    }
}

/// Basic-auth credentials for HTTPS. GitHub-style: username is your
/// account, `token` is a personal access token (or fine-grained token).
/// alt does not store, prompt for, or persist passwords — supply tokens.
#[derive(Debug, Clone)]
pub struct BasicAuth {
    pub username: String,
    pub token: String,
}

/// A handle to one git remote. Cheap to construct; each request goes out
/// on a connection of its own.
pub struct GitTransport {
    base_url: String,
    auth: Option<BasicAuth>,
    agent: ureq::Agent,
}

impl GitTransport {
    /// Build a transport bound to `base_url` — the repository URL, e.g.
    /// `https://github.com/user/repo.git`. Trailing slashes are stripped
    /// so the caller can be sloppy.
    pub fn new(base_url: impl Into<String>) -> Self {
        let mut url = base_url.into();
        while url.ends_with('/') {
            url.pop();
        }
        Self {
            base_url: url,
            auth: None,
            agent: agent(Duration::from_secs(30)),
        }
    }

    /// Attach Basic auth credentials. Without this the transport sends
    /// requests anonymously (fine for public read; push always needs auth).
    pub fn with_auth(mut self, auth: BasicAuth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Override how long the connection may stay silent (defaults to 30s).
    pub fn with_timeout(mut self, dur: Duration) -> Self {
        self.agent = agent(dur);
        self
    }

    /// The repository as a push names it to its server: the scheme, the host and
    /// the port as the `Host` header of the request carries them, and the path
    /// of the request before the service. They are read off the address once
    /// it is parsed for sending, not off the text of the URL: a port that is
    /// the one of the scheme, dots in a path, a host written in another form
    /// are named here as they are sent.
    pub fn pushed_to(&self) -> Result<String, TransportError> {
        let service = Service::ReceivePack.name();
        let url = format!("{}/{service}", self.base_url);
        let sent = self.agent.post(&url).request_url();
        let sent = sent.map_err(|e| TransportError::Other(e.to_string()))?;
        let of_scheme = match sent.scheme() {
            "http" => 80,
            "https" => 443,
            _ => 0,
        };
        let host = match sent.port() {
            Some(port) if port != of_scheme => format!("{}:{port}", sent.host()),
            _ => sent.host().to_owned(),
        };
        let path = sent.path();
        let path = path.strip_suffix(service).unwrap_or(path);
        let path = path.trim_end_matches('/');
        Ok(format!("{}://{host}{path}", sent.scheme()))
    }

    /// `GET <base>/info/refs?service=<svc>` with `Git-Protocol: version=2`.
    /// Returns the body, read into memory: one of more than `most` bytes is
    /// an error.
    pub fn info_refs(&self, svc: Service, most: u64) -> Result<Vec<u8>, TransportError> {
        let url = format!("{}/info/refs?service={}", self.base_url, svc.name());
        let mut req = self
            .agent
            .get(&url)
            .set("Git-Protocol", "version=2")
            .set("Accept", "*/*");
        if let Some(auth) = &self.auth {
            req = req.set("Authorization", &basic_auth_header(auth));
        }
        let resp = req.call().map_err(TransportError::from_ureq)?;
        read_body(resp, most)
    }

    /// `POST <base>/<svc>` with `body` as the request payload and the
    /// protocol v2 `Git-Protocol: version=2` + service-specific
    /// `Content-Type` / `Accept` headers. Returns the response body
    /// verbatim, read into memory: one of more than `most` bytes is an
    /// error. The caller pipes it back through [`alt-wire`] parsers.
    pub fn command(&self, svc: Service, body: &[u8], most: u64) -> Result<Vec<u8>, TransportError> {
        read_body(self.sent(svc, body)?, most)
    }

    /// [`Self::command`], with the response body read as it comes rather
    /// than into memory: for a pack, which is as large as the repository.
    pub fn command_stream(
        &self,
        svc: Service,
        body: &[u8],
    ) -> Result<Box<dyn Read + Send + Sync>, TransportError> {
        Ok(self.sent(svc, body)?.into_reader())
    }

    fn sent(&self, svc: Service, body: &[u8]) -> Result<ureq::Response, TransportError> {
        let url = format!("{}/{}", self.base_url, svc.name());
        let mut req = self
            .agent
            .post(&url)
            .set("Git-Protocol", "version=2")
            .set("Content-Type", svc.content_type())
            .set("Accept", svc.accept());
        if let Some(auth) = &self.auth {
            req = req.set("Authorization", &basic_auth_header(auth));
        }
        req.send_bytes(body).map_err(TransportError::from_ureq)
    }
}

/// Reasons a request fails. Wraps [`ureq::Error`] with a typed split for
/// the common "server gave us 4xx/5xx" case so the caller can react
/// without string-matching.
#[derive(Debug)]
pub enum TransportError {
    /// I/O failure on the wire (connection refused, TLS handshake error,
    /// truncated body). Wraps the underlying io::Error from ureq.
    Io(io::Error),
    /// The server returned a non-2xx HTTP status. `body` is captured so
    /// the caller can surface the error page (git servers send useful
    /// stderr text in the body of 401/403/404/500).
    HttpStatus { status: u16, body: String },
    /// ureq couldn't process the request at the transport layer (DNS,
    /// URL parse, TLS). Distinct from `HttpStatus` so the caller doesn't
    /// have to guess.
    Other(String),
    /// The response body has more bytes than the caller reads into memory.
    TooLarge { most: u64 },
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "transport io: {e}"),
            Self::HttpStatus { status, body } => write!(f, "http {status}: {body}"),
            Self::Other(msg) => write!(f, "transport: {msg}"),
            Self::TooLarge { most } => write!(f, "the answer has more than {most} bytes"),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::HttpStatus { .. } | Self::Other(_) | Self::TooLarge { .. } => None,
        }
    }
}

impl From<io::Error> for TransportError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl TransportError {
    fn from_ureq(e: ureq::Error) -> Self {
        match e {
            ureq::Error::Status(status, resp) => {
                let body = resp.into_string().unwrap_or_default();
                TransportError::HttpStatus { status, body }
            }
            ureq::Error::Transport(t) => TransportError::Other(t.to_string()),
        }
    }
}

/// An agent that gives up on a connection silent for `silence`, and puts no
/// bound on a whole exchange: a pack takes as long as its size and the link
/// make it, and Git's transport sets no such bound either.
///
/// Each request has a connection of its own. ureq clears the read and write
/// limits of a connection it keeps for another request and sets them again
/// only on a new one, so a request on a kept connection would wait without
/// end for a peer that does not answer.
fn agent(silence: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(silence)
        .timeout_read(silence)
        .timeout_write(silence)
        .max_idle_connections(0)
        .build()
}

fn basic_auth_header(auth: &BasicAuth) -> String {
    let raw = format!("{}:{}", auth.username, auth.token);
    format!("Basic {}", alt_base64::STANDARD.encode(raw.as_bytes()))
}

/// The body of `resp`, of `most` bytes at most.
fn read_body(resp: ureq::Response, most: u64) -> Result<Vec<u8>, TransportError> {
    let said = resp
        .header("Content-Length")
        .and_then(|n| n.parse::<u64>().ok());
    if said.is_some_and(|said| said > most) {
        return Err(TransportError::TooLarge { most });
    }
    let mut buf = Vec::new();
    resp.into_reader()
        .take(most.saturating_add(1))
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > most {
        return Err(TransportError::TooLarge { most });
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_auth_header_encodes_username_token() {
        let auth = BasicAuth {
            username: "alice".into(),
            token: "ghp_xxx".into(),
        };
        // base64("alice:ghp_xxx") = "YWxpY2U6Z2hwX3h4eA=="
        assert_eq!(basic_auth_header(&auth), "Basic YWxpY2U6Z2hwX3h4eA==");
    }

    #[test]
    fn service_names_map_to_git_endpoints() {
        assert_eq!(Service::UploadPack.name(), "git-upload-pack");
        assert_eq!(Service::ReceivePack.name(), "git-receive-pack");
        assert!(Service::UploadPack.content_type().ends_with("-request"));
        assert!(Service::ReceivePack.accept().ends_with("-result"));
    }

    /// What a push is sent with for each way of writing an address, read off
    /// the request as it arrives, is what [`GitTransport::pushed_to`] names.
    #[test]
    fn the_repository_a_push_names_is_the_host_and_the_path_its_request_carries() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
        let port = at.port();
        let heads = std::thread::spawn(move || {
            let mut heads = Vec::new();
            for _ in 0..7 {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut lines = Vec::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    lines.push(line.trim().to_owned());
                }
                let path = lines[0].split(' ').nth(1).unwrap().to_owned();
                let host = lines.iter().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("host")
                        .then(|| value.trim().to_owned())
                });
                heads.push((host.unwrap(), path));
                let answer = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                reader.get_mut().write_all(answer).unwrap();
            }
            heads
        });
        let written = [
            format!("http://127.0.0.1:{port}/team/repo"),
            format!("http://127.1:{port}/team/./repo/../repo/"),
            format!("http://LOCALHOST:{port}/a b/r\u{e9}po"),
            format!("http://[0:0:0:0:0:0:0:1]:{port}/r"),
            format!("http://b\u{fc}cher.example:{port}/r.git"),
            "http://host.example:80/r".to_owned(),
            "http://alice:secret@host.example/team/repo/".to_owned(),
        ];
        let mut named = Vec::new();
        for url in &written {
            let mut transport = GitTransport::new(url.as_str());
            // every name is this listener
            transport.agent = ureq::AgentBuilder::new()
                .resolver(move |_: &str| Ok(vec![at]))
                .build();
            named.push(transport.pushed_to().unwrap());
            transport.command(Service::ReceivePack, b"", 0).unwrap();
        }
        let heads = heads.join().unwrap();
        for ((host, path), named) in heads.iter().zip(&named) {
            let repository = path.strip_suffix("/git-receive-pack").unwrap();
            assert_eq!(&format!("http://{host}{repository}"), named);
        }
        assert_eq!(named[0], format!("http://127.0.0.1:{port}/team/repo"));
        assert_eq!(named[1], format!("http://127.0.0.1:{port}/team/repo"));
        assert_eq!(named[2], format!("http://localhost:{port}/a%20b/r%C3%A9po"));
        assert_eq!(named[3], format!("http://[::1]:{port}/r"));
        assert_eq!(
            named[4],
            format!("http://xn--bcher-kva.example:{port}/r.git")
        );
        assert_eq!(named[5], "http://host.example/r");
        assert_eq!(named[6], "http://host.example/team/repo");
    }

    #[test]
    fn base_url_trailing_slashes_are_stripped() {
        let t = GitTransport::new("https://github.com/user/repo.git///");
        assert_eq!(t.base_url, "https://github.com/user/repo.git");
    }
}

pub mod device;
pub mod native;
