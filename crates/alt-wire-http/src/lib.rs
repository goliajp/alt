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
//! - [`GitTransport`] — a connection-bound handle:
//!   - [`info_refs`] — `GET <base>/info/refs?service=<svc>` (the capability
//!     advertisement; pipe the bytes through
//!     [`alt_wire::parse_capability_advertisement`]).
//!   - [`command`] — `POST <base>/<svc>` with the encoded command body and
//!     the protocol v2 headers. Returns the response body verbatim for the
//!     pkt-line parsers in [`alt-wire`].
//! - [`BasicAuth`] — username + token for HTTPS auth (GitHub-style).
//! - [`Service`] — `UploadPack` for fetch / clone reads,
//!   `ReceivePack` for push writes.
//!
//! ## Scope (W2)
//!
//! Synchronous, blocking, one request at a time — same model as the rest
//! of alt (no async runtime). HTTP/2 is off (protocol v2 stateless POSTs
//! don't benefit; HTTP/1.1 keep-alive is enough). No credential helper
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

/// A connection-bound handle to one git remote. Cheap to construct; reuses
/// the underlying [`ureq::Agent`] across requests so TCP / TLS state is
/// kept warm.
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
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(30))
                .build(),
        }
    }

    /// Attach Basic auth credentials. Without this the transport sends
    /// requests anonymously (fine for public read; push always needs auth).
    pub fn with_auth(mut self, auth: BasicAuth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Override the request timeout (defaults to 30s).
    pub fn with_timeout(mut self, dur: Duration) -> Self {
        self.agent = ureq::AgentBuilder::new().timeout(dur).build();
        self
    }

    /// `GET <base>/info/refs?service=<svc>` with `Git-Protocol: version=2`.
    /// Returns the body as a `Vec<u8>` — small enough to buffer (capability
    /// advertisements are kB at most, even on large servers).
    pub fn info_refs(&self, svc: Service) -> Result<Vec<u8>, TransportError> {
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
        read_body(resp)
    }

    /// `POST <base>/<svc>` with `body` as the request payload and the
    /// protocol v2 `Git-Protocol: version=2` + service-specific
    /// `Content-Type` / `Accept` headers. Returns the response body
    /// verbatim — the caller pipes it back through [`alt-wire`] parsers.
    /// Pack-stream parsing (fetch's actual object stream) is a follow-up
    /// step; this transport just moves bytes.
    pub fn command(&self, svc: Service, body: &[u8]) -> Result<Vec<u8>, TransportError> {
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
        let resp = req.send_bytes(body).map_err(TransportError::from_ureq)?;
        read_body(resp)
    }
}

/// Reasons a request fails. Wraps [`ureq::Error`] with a typed split for
/// the common "server gave us 4xx/5xx" case so the caller can react
/// without string-matching.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// I/O failure on the wire (connection refused, TLS handshake error,
    /// truncated body). Wraps the underlying io::Error from ureq.
    #[error("transport io: {0}")]
    Io(#[from] io::Error),
    /// The server returned a non-2xx HTTP status. `body` is captured so
    /// the caller can surface the error page (git servers send useful
    /// stderr text in the body of 401/403/404/500).
    #[error("http {status}: {body}")]
    HttpStatus { status: u16, body: String },
    /// ureq couldn't process the request at the transport layer (DNS,
    /// URL parse, TLS). Distinct from `HttpStatus` so the caller doesn't
    /// have to guess.
    #[error("transport: {0}")]
    Other(String),
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

fn basic_auth_header(auth: &BasicAuth) -> String {
    let raw = format!("{}:{}", auth.username, auth.token);
    format!("Basic {}", base64_encode(raw.as_bytes()))
}

fn read_body(resp: ureq::Response) -> Result<Vec<u8>, TransportError> {
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf)?;
    Ok(buf)
}

/// Tiny base64 encoder — RFC 4648 standard alphabet, padding, no newlines.
/// Pulled inline so the crate doesn't take a `base64` dep just for the
/// Basic-auth header (one call per request, performance irrelevant).
fn base64_encode(input: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8) | (input[i + 2] as u32);
        out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3f) as usize] as char);
        out.push(ALPHA[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let n = (input[i] as u32) << 16;
        out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
        out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3f) as usize] as char);
        out.push('=');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        // RFC 4648 §10
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

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

    #[test]
    fn base_url_trailing_slashes_are_stripped() {
        let t = GitTransport::new("https://github.com/user/repo.git///");
        assert_eq!(t.base_url, "https://github.com/user/repo.git");
    }
}
