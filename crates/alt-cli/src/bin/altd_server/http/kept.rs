//! An answer whose head goes out before its body is made, as `git
//! http-backend` sends the head of an `upload-pack` before the pack is
//! made: the pack of a large repository is made for minutes, and a
//! client that hears nothing for so long takes the server for gone. The
//! answer to a push is begun so too while its pack is taken in, as
//! `receive-pack` sends its keepalive then.
//!
//! The handler says how its answer begins once nothing is left that would
//! answer it with another status ([`Request::begin`]). The thread of the
//! connection then waits for the rest of it for one `every`, and where it
//! comes in that time it goes out as any answer does. Where it does not,
//! the head goes out with what begins the body, in chunks, and the
//! keepalive of `upload-pack`, a packet of the pack's side band with no
//! bytes in it, follows each `every` until the rest is there. A peer of
//! HTTP/1.0 reads no chunks: its body goes out as it is, and ends where the
//! connection does. What would
//! have answered with another status from then on is told in the side
//! band of errors, as `upload-pack` tells what stopped it.

use std::io::Write;
use std::net::TcpStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::back::{Back, Way};
use super::head::Header;
use super::request::Request;
use super::response::Outgoing;

/// the keepalive `upload-pack` sends while it makes a pack
const KEEPALIVE: &[u8] = b"0005\x01";

/// How an answer of status 200 begins.
pub(crate) struct Begun {
    headers: Vec<Header>,
    body: Vec<u8>,
    every: Duration,
    /// the body goes out in chunks; where not, it ends where the connection does
    chunked: bool,
    /// the connection ends behind the answer
    last: bool,
}

/// What of an answer begun went out, as the worker that answers it and the
/// thread of its connection both see it.
#[derive(Default)]
pub(super) struct Out {
    /// the bytes the body begins with, where it was begun
    begun: Option<u64>,
    /// the answer is handed back: nothing more goes out ahead of it
    answered: bool,
    /// the head went out
    head: bool,
    /// the keepalives that went out
    beats: u64,
}

impl Way {
    /// The status the peer is sent, and the bytes of the body, for
    /// `outgoing`, which is handed back now.
    pub(super) fn answered(&self, outgoing: &Outgoing) -> (u16, u64) {
        let mut out = self.out.lock().expect("what went out");
        out.answered = true;
        match out.begun {
            Some(begun) if out.head => {
                let beats = out.beats * KEEPALIVE.len() as u64;
                (200, begun + beats + outgoing.rest(begun))
            }
            _ => (outgoing.status, outgoing.length),
        }
    }
}

impl Request {
    /// Tells the thread of the connection that the answer is a 200 with
    /// `headers` whose body begins with `body`, and that a keepalive may
    /// go out each `every` until the rest of it is there.
    pub(crate) fn begin(&self, headers: Vec<Header>, body: Vec<u8>, every: Duration) {
        self.back.out.lock().expect("what went out").begun = Some(body.len() as u64);
        let begun = Begun {
            headers,
            body,
            every,
            chunked: !self.head.old,
            last: self.head.last || self.head.old || !self.over(),
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
    /// the answer was handed back, and nothing more goes out ahead of it
    stopped: bool,
}

impl Kept {
    pub(super) fn new(begun: Box<Begun>) -> Self {
        let next = Instant::now() + begun.every;
        Kept {
            begun,
            next,
            sent: false,
            broken: false,
            stopped: false,
        }
    }

    /// When the next keepalive is due; never where the connection broke or
    /// the answer is there.
    pub(super) fn due(&self) -> Option<Instant> {
        (!self.broken && !self.stopped).then_some(self.next)
    }

    /// The bytes of the body that went out with the head.
    pub(super) fn begun(&self) -> u64 {
        self.begun.body.len() as u64
    }

    /// Whether the body goes out in chunks.
    pub(super) fn chunked(&self) -> bool {
        self.begun.chunked
    }

    /// Whether the connection ends behind the answer, once its head went out.
    pub(super) fn last(&self) -> bool {
        self.begun.last
    }

    /// Sends a keepalive on `watched`, behind the head and what begins
    /// the body where those did not go out yet, each write within `window`;
    /// none where the answer was handed back meanwhile. What goes out is
    /// counted in `told`.
    pub(super) fn beat(&mut self, watched: &TcpStream, window: Duration, told: &Mutex<Out>) {
        let mut counted = told.lock().expect("what went out");
        if counted.answered {
            self.stopped = true;
            return;
        }
        counted.head = true;
        counted.beats += 1;
        drop(counted);
        let mut out = Vec::new();
        if !self.sent {
            out.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
            for header in &self.begun.headers {
                let line = format!("{}: {}\r\n", header.field.as_str(), header.value);
                out.extend_from_slice(line.as_bytes());
            }
            if self.begun.chunked {
                out.extend_from_slice(b"Transfer-Encoding: chunked\r\n");
            }
            if self.begun.last {
                out.extend_from_slice(b"Connection: close\r\n");
            }
            out.extend_from_slice(b"\r\n");
            piece(&mut out, &self.begun.body, self.begun.chunked);
            self.sent = true;
        }
        piece(&mut out, KEEPALIVE, self.begun.chunked);
        let mut to = watched;
        let wrote = watched.set_write_timeout(Some(window));
        self.broken = wrote.and_then(|()| to.write_all(&out)).is_err();
        self.next = Instant::now() + self.begun.every;
    }
}

/// `bytes` as a piece of a body, a chunk where it is `chunked`. Nothing
/// where there are none: a chunk of nothing would end the body.
pub(super) fn piece(out: &mut Vec<u8>, bytes: &[u8], chunked: bool) {
    match chunked {
        _ if bytes.is_empty() => {}
        true => chunk(out, bytes),
        false => out.extend_from_slice(bytes),
    }
}

/// `bytes` as a chunk of a body sent in chunks.
pub(super) fn chunk(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(format!("{:x}\r\n", bytes.len()).as_bytes());
    out.extend_from_slice(bytes);
    out.extend_from_slice(b"\r\n");
}
