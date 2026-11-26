//! An answer whose head goes out before its body is made, as `git
//! http-backend` sends the head of an `upload-pack` before the pack is
//! made: the pack of a large repository is made for minutes, and a
//! client that hears nothing for so long takes the server for gone.
//!
//! The handler says how its answer begins once nothing is left that would
//! answer it with another status ([`Request::begin`]). The thread of the
//! connection then waits for the rest of it for one `every`, and where it
//! comes in that time it goes out as any answer does. Where it does not,
//! the head goes out with what begins the body, in chunks, and the
//! keepalive of `upload-pack`, a packet of the pack's side band with no
//! bytes in it, follows each `every` until the rest is there. What would
//! have answered with another status from then on is told in the side
//! band of errors, as `upload-pack` tells what stopped it.

use std::io::Write;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use super::back::Back;
use super::head::Header;
use super::request::Request;

/// the keepalive `upload-pack` sends while it makes a pack
const KEEPALIVE: &[u8] = b"0005\x01";

/// How an answer of status 200 begins.
pub(crate) struct Begun {
    headers: Vec<Header>,
    body: Vec<u8>,
    every: Duration,
}

impl Request {
    /// Tells the thread of the connection that the answer is a 200 with
    /// `headers` whose body begins with `body`, and that a keepalive may
    /// go out each `every` until the rest of it is there.
    pub(crate) fn begin(&self, headers: Vec<Header>, body: Vec<u8>, every: Duration) {
        let begun = Begun {
            headers,
            body,
            every,
        };
        self.back.send(Back::Begun(Box::new(begun)));
    }
}

/// An answer begun, at the thread of its connection.
pub(super) struct Kept {
    begun: Box<Begun>,
    next: Instant,
    /// its head went out
    pub(super) sent: bool,
    /// a write to the peer failed
    pub(super) broken: bool,
}

impl Kept {
    pub(super) fn new(begun: Box<Begun>) -> Self {
        let next = Instant::now() + begun.every;
        Kept {
            begun,
            next,
            sent: false,
            broken: false,
        }
    }

    /// When the next keepalive is due; never where the connection broke.
    pub(super) fn due(&self) -> Option<Instant> {
        (!self.broken).then_some(self.next)
    }

    /// The bytes of the body that went out with the head.
    pub(super) fn begun(&self) -> u64 {
        self.begun.body.len() as u64
    }

    /// Sends a keepalive on `watched`, behind the head and what begins
    /// the body where those did not go out yet, each write within `window`.
    pub(super) fn beat(&mut self, watched: &TcpStream, window: Duration) {
        let mut out = Vec::new();
        if !self.sent {
            out.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
            for header in &self.begun.headers {
                let line = format!("{}: {}\r\n", header.field.as_str(), header.value);
                out.extend_from_slice(line.as_bytes());
            }
            out.extend_from_slice(b"Transfer-Encoding: chunked\r\n\r\n");
            chunk(&mut out, &self.begun.body);
            self.sent = true;
        }
        chunk(&mut out, KEEPALIVE);
        let mut to = watched;
        let wrote = watched.set_write_timeout(Some(window));
        self.broken = wrote.and_then(|()| to.write_all(&out)).is_err();
        self.next = Instant::now() + self.begun.every;
    }
}

/// `bytes` as a chunk of a body sent in chunks.
pub(super) fn chunk(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(format!("{:x}\r\n", bytes.len()).as_bytes());
    out.extend_from_slice(bytes);
    out.extend_from_slice(b"\r\n");
}
