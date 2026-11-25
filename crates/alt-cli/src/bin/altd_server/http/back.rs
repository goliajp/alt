//! What goes between a request at its worker and the thread of its
//! connection.
//!
//! The request hands back what the thread is to do for it: receive its
//! body, write its answer. The thread, which waits for that with nothing
//! to do, looks every so often whether the peer has closed its side, and
//! says so where the worker sees it: a push that waits for what other
//! pushes have, or is being indexed, is gone on with for nobody then.

use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use super::paced::Source;
use super::request::Request;
use super::response::Outgoing;

/// how often the thread of a connection looks whether its peer is gone
const LOOK: Duration = Duration::from_millis(100);

/// What a request hands to the thread of its connection.
pub(super) enum Back {
    /// the answer, with the connection it goes out on
    Answer(Box<(Source, Outgoing)>),
    /// the request, for its body to be received within so many bytes
    Receive(Box<Request>, u64),
}

/// The way from a request to the thread of its connection.
#[derive(Clone)]
pub(super) struct Way {
    to: Sender<Back>,
    /// set once the peer has closed its side
    pub(super) gone: Arc<AtomicBool>,
}

impl Way {
    /// the way, and where the thread of the connection hears of what is handed back
    pub(super) fn new() -> (Way, Receiver<Back>) {
        let (to, returned) = mpsc::channel();
        let gone = Arc::default();
        (Way { to, gone }, returned)
    }

    pub(super) fn send(&self, back: Back) {
        // the thread is gone where its connection is
        let _ = self.to.send(back);
    }

    /// What the request hands back next, or nothing where it is gone
    /// without an answer. While none comes, `watched`, a second handle to
    /// the connection, is looked at for a peer that closed its side.
    pub(super) fn returned(&self, returned: &Receiver<Back>, watched: &TcpStream) -> Option<Back> {
        loop {
            match returned.recv_timeout(LOOK) {
                Ok(back) => return Some(back),
                Err(RecvTimeoutError::Disconnected) => return None,
                Err(RecvTimeoutError::Timeout) => {
                    if alt_os::socket::peer_closed(watched) {
                        self.gone.store(true, Relaxed);
                    }
                }
            }
        }
    }
}
