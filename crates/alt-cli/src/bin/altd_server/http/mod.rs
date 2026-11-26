//! HTTP/1.1 as `altd-server` speaks it, over the standard library alone.
//!
//! Every connection has a thread of its own that reads the heads of its
//! requests, receives their bodies and writes the answers to them; a
//! request waits for one of the workers with its head read, and again
//! with its body received when its handler asked for that. So a worker
//! waits for no peer: not for a head, not for a body, not for a peer to
//! take an answer, not for a connection to end.
//!
//! What a peer keeps is its connection and the thread of it, for as long
//! as it goes on with what it is doing (see [`paced`]), and the file its
//! body is received into or its answer is sent from. Those have limits
//! of their own: the connections open at once, of which an address may
//! have so many, and the bytes of the bodies received. A connection
//! above a limit is not taken.
//!
//! TLS is in front of this, with whatever else a public listener needs.

mod back;
mod chunks;
mod head;
mod kept;
mod paced;
mod peers;
mod request;
mod response;

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use back::{Back, Heard};

pub(crate) use alt_shares::{Share, Shares};
pub(crate) use head::{Header, Method};
pub(crate) use paced::Limits;
pub(crate) use peers::{LOOPBACK, Network, networks};
pub(crate) use request::Request;
pub(crate) use response::{Response, StatusCode};

#[derive(Default)]
struct Waiting {
    requests: VecDeque<Request>,
    /// the calls of `recv` that are to return without a request
    unblocked: usize,
    /// every call that finds no request returns without one
    closed: bool,
}

struct Shared {
    waiting: Mutex<Waiting>,
    arrived: Condvar,
    open: AtomicUsize,
    /// the connections open from each address that has a limit
    from: Mutex<HashMap<IpAddr, usize>>,
    /// the bytes of the bodies received and still held, by the address they came from
    held: Arc<Shares>,
    limits: Limits,
    /// the networks whose peers are of this host, and counted for nothing
    here: &'static [Network],
    /// those of them whose peers say whose a request is
    proxies: &'static [Network],
}

pub(crate) struct Server {
    address: SocketAddr,
    shared: Arc<Shared>,
}

/// One of the connections that are open, for as long as it is, the
/// address it counts for and the one it comes from.
struct Open(Arc<Shared>, Option<IpAddr>, IpAddr);

impl Drop for Open {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
        if let Some(address) = self.1 {
            let mut from = self.0.from.lock().expect("addresses");
            if let Some(open) = from.get_mut(&address) {
                *open -= 1;
                if *open == 0 {
                    from.remove(&address);
                }
            }
        }
    }
}

impl Shared {
    /// Counts a connection from `peer` in, where the server and the
    /// address have room for one more.
    fn open(self: &Arc<Self>, peer: IpAddr, most: usize) -> Option<Open> {
        if self.open.fetch_add(1, Ordering::AcqRel) >= most {
            drop(Open(Arc::clone(self), None, peer));
            return None;
        }
        let address = peers::counted(peer, self.here);
        if let Some(address) = address {
            let mut from = self.from.lock().expect("addresses");
            let open = from.entry(address).or_insert(0);
            if *open >= self.limits.each {
                drop(from);
                drop(Open(Arc::clone(self), None, peer));
                return None;
            }
            *open += 1;
        }
        Some(Open(Arc::clone(self), address, peer))
    }

    fn enqueue(&self, request: Request) {
        let mut waiting = self.waiting.lock().expect("requests");
        waiting.requests.push_back(request);
        self.arrived.notify_one();
    }
}

impl Server {
    /// Listens at `address` for `most` connections at once, whose peers are
    /// given `limits`; those of the networks `here` are of this host, and
    /// those of `proxies` among them say whose their requests are. `large`
    /// bodies at once may pass the part of their address.
    pub(crate) fn listen(
        address: &str,
        limits: Limits,
        (most, large): (usize, usize),
        here: &'static [Network],
        proxies: &'static [Network],
    ) -> io::Result<Server> {
        let listener = TcpListener::bind(address)?;
        let shared = Arc::new(Shared {
            waiting: Mutex::default(),
            arrived: Condvar::new(),
            open: AtomicUsize::new(0),
            from: Mutex::default(),
            held: Shares::of(limits.spool, large),
            limits,
            here,
            proxies,
        });
        let server = Server {
            address: listener.local_addr()?,
            shared: Arc::clone(&shared),
        };
        std::thread::Builder::new()
            .name("accept".into())
            .spawn(move || {
                // a connection that cannot be taken is most often one too
                // many files open, and stays in the queue: the next try
                // waits, longer each time, for one to be closed
                let mut pause = Duration::ZERO;
                for stream in listener.incoming() {
                    let Ok(stream) = stream else {
                        pause = (pause * 2).clamp(Duration::from_millis(5), Duration::from_secs(1));
                        std::thread::sleep(pause);
                        continue;
                    };
                    pause = Duration::ZERO;
                    let peer = stream.peer_addr().ok();
                    let Some(open) = peer.and_then(|peer| shared.open(peer.ip(), most)) else {
                        continue;
                    };
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
    /// [`Server::unblock`] lets go of, or that finds none once the server is
    /// [closed](Server::close).
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
            if waiting.closed {
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

    /// Has every call of [`Server::recv`] that finds no request return.
    pub(crate) fn close(&self) {
        self.shared.waiting.lock().expect("requests").closed = true;
        self.shared.arrived.notify_all();
    }
}

/// Reads the requests of one connection, one behind the other, hands
/// each to the workers and writes what they answer.
fn connection(open: Open, stream: TcpStream) {
    // an answer is written in a few pieces, and none waits for the peer to
    // acknowledge the one before
    if stream.set_nodelay(true).is_err() {
        return;
    }
    let Ok(watched) = stream.try_clone() else {
        return;
    };
    let shared = &open.0;
    let mut source = paced::Source::new(stream, shared.limits, open.1);
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
                let _ = refusal.outgoing(true, false).send(&mut source);
                return source.close();
            }
        };
        // a proxy says whose the request is
        let forwarded = head
            .headers
            .iter()
            .filter(|h| h.field.equiv("X-Forwarded-For"));
        let forwarded = forwarded.map(|h| h.value.as_str());
        source.from = peers::asked_from(open.2, forwarded, shared.proxies, shared.here);
        let (way, returned) = back::Way::new();
        shared.enqueue(Request::new(head, source, way.clone()));
        let mut kept: Option<kept::Kept> = None;
        let (back, answer) = loop {
            match way.returned(&returned, &watched, kept.as_ref().and_then(kept::Kept::due)) {
                Heard::Back(Back::Answer(answered)) => break *answered,
                Heard::Back(Back::Receive(mut request, most)) => {
                    // one whose body has no file to go to is let go of,
                    // and answers itself
                    if request.spool(most, &shared.held).is_ok() {
                        shared.enqueue(*request);
                    }
                }
                Heard::Back(Back::Begun(begun)) => kept = Some(kept::Kept::new(begun)),
                Heard::Due => {
                    for kept in kept.iter_mut() {
                        kept.beat(&watched, shared.limits.window, &way.out);
                        // the answer is made for nobody once it cannot go out
                        if kept.broken {
                            way.gone.store(true, Ordering::Relaxed);
                        }
                    }
                }
                Heard::Gone => return,
            }
        };
        source = back;
        let last = answer.last || kept.as_ref().is_some_and(|kept| kept.sent && kept.last());
        let sent = match &kept {
            Some(kept) if kept.broken => return source.close(),
            Some(kept) if kept.sent => answer.send_rest(&mut source, kept.begun(), kept.chunked()),
            _ => answer.send(&mut source),
        };
        if sent.is_err() || last {
            return source.close();
        }
    }
}

#[cfg(test)]
mod tests;
