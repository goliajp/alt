//! One accepted connection: bounded HTTP/1 framing around the router.
use super::{App, sources::Place};
use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderValue, Method, Request, Response, header},
};
use bytes::Bytes;
use hyper::{
    body::{Body as _, Frame, Incoming, SizeHint},
    server::conn::http1,
    service::{Service, service_fn},
};
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use std::{
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::net::TcpStream;

/// Marks a response whose handler wrote the Content-Length header itself.
#[derive(Clone)]
pub(super) struct DeclaredLength;

/// How long a connection lives: the time a connection is given, counted again
/// from every piece of a request body that comes on it, from everything that is
/// sent on it, and from the answer to a request of it. A connection on which a
/// body is sent or a response is read slowly and goes on is not over while it
/// does, nor is one whose request is being answered; one on which nothing moves
/// and nothing is asked is over when its time is.
pub(super) struct Life {
    each: Duration,
    until: Mutex<tokio::time::Instant>,
    /// the requests of the connection that have no answer yet
    asked: AtomicUsize,
}
impl Life {
    pub(super) fn moved(&self) {
        *self.until.lock().expect("connection") = tokio::time::Instant::now() + self.each;
    }
    fn until(&self) -> tokio::time::Instant {
        *self.until.lock().expect("connection")
    }
}

/// A request from when it is handed to its handler to when the handler answers.
struct Asked(Arc<Life>);
impl Asked {
    fn of(life: &Arc<Life>) -> Self {
        life.asked.fetch_add(1, Ordering::AcqRel);
        Asked(life.clone())
    }
}
impl Drop for Asked {
    fn drop(&mut self) {
        self.0.asked.fetch_sub(1, Ordering::AcqRel);
        self.0.moved();
    }
}

pub(super) async fn serve(socket: TcpStream, peer: SocketAddr, router: Router, state: &Arc<App>) {
    let (options, sources) = (&state.options, state.sources.clone());
    let life = Arc::new(Life {
        each: options.connection_timeout,
        until: Mutex::new(tokio::time::Instant::now() + options.connection_timeout),
        asked: AtomicUsize::new(0),
    });
    let lives = life.clone();
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(options.header_timeout)
        .max_headers(options.headers)
        .max_buf_size(options.header_bytes);
    let router = TowerToHyperService::new(router);
    let service = service_fn(move |request: Request<Incoming>| {
        let bodiless = request.body().is_end_stream();
        // of the address the request is from, where a proxy says whose it is
        let asked = sources.asked(peer, request.headers()).map(|place| {
            let (parts, body) = request.into_parts();
            let ended = Arc::new(AtomicBool::new(body.is_end_stream()));
            let life = lives.clone();
            let received = Received {
                body,
                ended: ended.clone(),
                life: life.clone(),
            };
            let mut request = Request::from_parts(parts, received);
            request.extensions_mut().insert(ConnectInfo(peer));
            let head = request.method() == Method::HEAD;
            (router.call(request), Asked::of(&life), (head, ended), place)
        });
        async move {
            let Ok((response, asked, (head, ended), place)) = asked else {
                let refused = super::respond::crowded().map(Body::new);
                return Ok(outgoing(false, bodiless, refused, None));
            };
            let response = response.await?;
            drop(asked);
            let ended = ended.load(Ordering::Acquire);
            Ok::<_, std::convert::Infallible>(outgoing(head, ended, response, place))
        }
    });
    let socket = super::paced::Paced::new(socket, life.clone(), options.body_timeout);
    let serving = builder.serve_connection(TokioIo::new(socket), service);
    tokio::pin!(serving);
    loop {
        tokio::select! {
            _ = &mut serving => break,
            _ = tokio::time::sleep_until(life.until()) => {
                // how long a request is answered for is its handler's to bound
                if life.asked.load(Ordering::Acquire) > 0 {
                    life.moved();
                } else if life.until() <= tokio::time::Instant::now() {
                    break;
                }
            }
        }
    }
}

/// The request body as the handler receives it; notes that it was read to its end.
struct Received {
    body: Incoming,
    ended: Arc<AtomicBool>,
    life: Arc<Life>,
}
impl hyper::body::Body for Received {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        let this = self.get_mut();
        let frame = Pin::new(&mut this.body).poll_frame(cx);
        if matches!(frame, Poll::Ready(Some(Ok(_)))) {
            this.life.moved();
        }
        if matches!(frame, Poll::Ready(None)) || this.body.is_end_stream() {
            this.ended.store(true, Ordering::Release);
        }
        frame
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

/// The router writes the length of every sized body into the headers and answers
/// HEAD with an empty one. The connection writes that length itself, after its own
/// headers, so only a length the handler declared stays in the header map.
///
/// A response to a request whose body was not read to its end is the last one on
/// its connection: the unread rest cannot be told apart from a next request. The
/// response says so, or the client sends its next request into a closed connection.
/// A request that is refused unread and brought no body leaves nothing behind: its
/// connection, which is a proxy's, goes on for the next caller.
fn outgoing(
    head: bool,
    ended: bool,
    response: Response<Body>,
    place: Option<Place>,
) -> Response<Outgoing> {
    let (mut parts, body) = response.into_parts();
    if !ended {
        parts
            .headers
            .insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
    let mut length = None;
    if parts.extensions.get::<DeclaredLength>().is_none() {
        let written = parts.headers.remove(header::CONTENT_LENGTH);
        if head {
            length = written.and_then(|value| value.to_str().ok()?.parse().ok());
        }
    }
    let body = Outgoing {
        body,
        length,
        _place: place,
    };
    Response::from_parts(parts, body)
}

struct Outgoing {
    body: Body,
    /// the length of the body a HEAD request did not get
    length: Option<u64>,
    /// what the request has of its address's, to the end of its response
    _place: Option<Place>,
}
impl hyper::body::Body for Outgoing {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        Pin::new(&mut self.get_mut().body).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.length.is_none() && self.body.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        match self.length {
            Some(length) => SizeHint::with_exact(length),
            None => self.body.size_hint(),
        }
    }
}
