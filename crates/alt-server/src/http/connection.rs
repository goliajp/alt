//! One accepted connection: bounded HTTP/1 framing around the router.
use super::HttpOptions;
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
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::net::TcpStream;

/// Marks a response whose handler wrote the Content-Length header itself.
#[derive(Clone)]
pub(super) struct DeclaredLength;

/// How long a connection lives: the time a connection is given, counted again
/// from every piece of a body that moves on it. A connection on which a body is
/// sent or read slowly and goes on is not over while it does; one on which
/// nothing moves is over when its time is.
struct Life {
    each: Duration,
    until: Mutex<tokio::time::Instant>,
}
impl Life {
    fn moved(&self) {
        *self.until.lock().expect("connection") = tokio::time::Instant::now() + self.each;
    }
    fn until(&self) -> tokio::time::Instant {
        *self.until.lock().expect("connection")
    }
}

/// What a response body has to keep to while its peer reads it: in each window
/// of the time the peer took, as much as a request body has to bring in one.
/// The time the handler took to make a piece is not the peer's.
struct Pace {
    window: Duration,
    taken: Duration,
    moved: u64,
    handed: Option<tokio::time::Instant>,
}
impl Pace {
    /// Called as the next piece is asked for; false where the peer was too slow.
    fn asked(&mut self, now: tokio::time::Instant) -> bool {
        if let Some(handed) = self.handed.take() {
            self.taken += now - handed;
        }
        if self.taken >= self.window {
            if self.moved < super::body::least(self.window) {
                return false;
            }
            (self.taken, self.moved) = (Duration::ZERO, 0);
        }
        true
    }
}

pub(super) async fn serve(
    socket: TcpStream,
    peer: SocketAddr,
    router: Router,
    options: &HttpOptions,
) {
    let life = Arc::new(Life {
        each: options.connection_timeout,
        until: Mutex::new(tokio::time::Instant::now() + options.connection_timeout),
    });
    let window = options.body_timeout;
    let lives = life.clone();
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(options.header_timeout)
        .max_headers(options.headers)
        .max_buf_size(options.header_bytes);
    let router = TowerToHyperService::new(router);
    let service = service_fn(move |request: Request<Incoming>| {
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
        let response = router.call(request);
        async move {
            let response = response.await?;
            let pace = Pace {
                window,
                taken: Duration::ZERO,
                moved: 0,
                handed: None,
            };
            let ended = ended.load(Ordering::Acquire);
            Ok::<_, std::convert::Infallible>(outgoing(head, ended, response, life, pace))
        }
    });
    let serving = builder.serve_connection(TokioIo::new(socket), service);
    tokio::pin!(serving);
    loop {
        tokio::select! {
            _ = &mut serving => break,
            _ = tokio::time::sleep_until(life.until()) => {
                if life.until() <= tokio::time::Instant::now() {
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
fn outgoing(
    head: bool,
    ended: bool,
    response: Response<Body>,
    life: Arc<Life>,
    pace: Pace,
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
        life,
        pace,
    };
    Response::from_parts(parts, body)
}

struct Outgoing {
    body: Body,
    /// the length of the body a HEAD request did not get
    length: Option<u64>,
    life: Arc<Life>,
    pace: Pace,
}
impl hyper::body::Body for Outgoing {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let this = self.get_mut();
        // the piece before this one is with the peer, or on its way there
        if !this.pace.asked(tokio::time::Instant::now()) {
            let slow =
                std::io::Error::new(std::io::ErrorKind::TimedOut, "response read too slowly");
            return Poll::Ready(Some(Err(axum::Error::new(slow))));
        }
        let frame = Pin::new(&mut this.body).poll_frame(cx);
        if let Poll::Ready(Some(Ok(frame))) = &frame {
            this.life.moved();
            this.pace.moved += frame.data_ref().map_or(0, |data| data.len() as u64);
            this.pace.handed = Some(tokio::time::Instant::now());
        }
        frame
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
