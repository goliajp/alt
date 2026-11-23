//! HTTP/1.1 as `altd-server` speaks it, over the standard library alone.
//!
//! Every connection has a thread of its own that reads the heads of its
//! requests; a request whose head is read waits for one of the workers,
//! which reads its body and answers it. A connection that sends nothing,
//! or sends slowly, so keeps its own thread and no worker, and is let go
//! of when the time it is given is over (see [`paced`]). The connections
//! open at once have a limit, and one above it is not taken.
//!
//! TLS is in front of this, with whatever else a public listener needs.

mod head;
mod paced;
mod request;
mod response;

use std::collections::VecDeque;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub(crate) use head::{Header, Method};
pub(crate) use request::Request;
pub(crate) use response::{Response, StatusCode};

/// What a peer is given to begin with, where `ALT_SERVER_IO_TIMEOUT_MS`
/// says nothing else.
const GRACE: Duration = Duration::from_secs(20);
/// The connections open at once, where `ALT_SERVER_MAX_CONNECTIONS` says
/// nothing else.
const CONNECTIONS: usize = 256;

fn told<T: std::str::FromStr + PartialOrd + Default>(name: &str) -> Option<T> {
    let value = std::env::var(name).ok()?.parse().ok()?;
    (value > T::default()).then_some(value)
}

#[derive(Default)]
struct Waiting {
    requests: VecDeque<Request>,
    /// the calls of `recv` that are to return without a request
    unblocked: usize,
}

struct Shared {
    waiting: Mutex<Waiting>,
    arrived: Condvar,
    open: AtomicUsize,
    grace: Duration,
}

pub(crate) struct Server {
    address: SocketAddr,
    shared: Arc<Shared>,
}

/// One of the connections that are open, for as long as it is.
struct Open(Arc<Shared>);

impl Drop for Open {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Server {
    /// Listens at `address` and takes connections from then on, as the
    /// environment bounds them.
    pub(crate) fn http(address: &str) -> io::Result<Server> {
        let grace = told("ALT_SERVER_IO_TIMEOUT_MS").map_or(GRACE, Duration::from_millis);
        let most = told("ALT_SERVER_MAX_CONNECTIONS").unwrap_or(CONNECTIONS);
        Server::listen(address, grace, most)
    }

    /// Listens at `address` for `most` connections at once, whose peers are
    /// given `grace`.
    fn listen(address: &str, grace: Duration, most: usize) -> io::Result<Server> {
        let listener = TcpListener::bind(address)?;
        let shared = Arc::new(Shared {
            waiting: Mutex::default(),
            arrived: Condvar::new(),
            open: AtomicUsize::new(0),
            grace,
        });
        let server = Server {
            address: listener.local_addr()?,
            shared: Arc::clone(&shared),
        };
        std::thread::Builder::new()
            .name("accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    if shared.open.fetch_add(1, Ordering::AcqRel) >= most {
                        drop(Open(Arc::clone(&shared)));
                        continue;
                    }
                    let open = Open(Arc::clone(&shared));
                    let serve = move || connection(open, stream);
                    let thread = std::thread::Builder::new().stack_size(256 * 1024);
                    // one that cannot be started lets go of the connection
                    let _ = thread.name("connection".into()).spawn(serve);
                }
            })?;
        Ok(server)
    }

    pub(crate) fn server_addr(&self) -> SocketAddr {
        self.address
    }

    /// The next request whose head was read, or an error for a call that
    /// [`Server::unblock`] lets go of.
    pub(crate) fn recv(&self) -> io::Result<Request> {
        let mut waiting = self.shared.waiting.lock().expect("requests");
        loop {
            if let Some(request) = waiting.requests.pop_front() {
                return Ok(request);
            }
            if waiting.unblocked > 0 {
                waiting.unblocked -= 1;
                return Err(io::ErrorKind::Interrupted.into());
            }
            waiting = self.shared.arrived.wait(waiting).expect("requests");
        }
    }

    /// Has one call of [`Server::recv`] that finds no request return.
    pub(crate) fn unblock(&self) {
        self.shared.waiting.lock().expect("requests").unblocked += 1;
        self.shared.arrived.notify_all();
    }
}

/// Reads the requests of one connection, one behind the other, and hands
/// each to the workers.
fn connection(open: Open, stream: TcpStream) {
    // an answer is written in a few pieces, and none waits for the peer to
    // acknowledge the one before
    if stream.set_nodelay(true).is_err() {
        return;
    }
    let shared = &open.0;
    let mut source = paced::Source::new(stream, shared.grace);
    loop {
        let head = match head::Head::read(&mut source) {
            Ok(Some(head)) => head,
            Ok(None) => return,
            Err(error) => {
                let status = match error.kind() {
                    io::ErrorKind::InvalidData => 400,
                    io::ErrorKind::TimedOut => 408,
                    _ => return,
                };
                let refusal = Response::from_string(error.to_string()).with_status_code(status);
                let _ = refusal.send(&mut source, true, false);
                return source.close();
            }
        };
        let (back, returned) = mpsc::channel();
        let request = Request::new(head, source, back);
        shared
            .waiting
            .lock()
            .expect("requests")
            .requests
            .push_back(request);
        shared.arrived.notify_one();
        match returned.recv() {
            Ok(Some(next)) => source = next,
            _ => return,
        }
    }
}

#[cfg(test)]
mod tests;
