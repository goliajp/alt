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
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};
use tokio::net::TcpStream;

/// Marks a response whose handler wrote the Content-Length header itself.
#[derive(Clone)]
pub(super) struct DeclaredLength;

pub(super) async fn serve(
    socket: TcpStream,
    peer: SocketAddr,
    router: Router,
    options: &HttpOptions,
) {
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
        let received = Received {
            body,
            ended: ended.clone(),
        };
        let mut request = Request::from_parts(parts, received);
        request.extensions_mut().insert(ConnectInfo(peer));
        let head = request.method() == Method::HEAD;
        let response = router.call(request);
        async move {
            let response = response.await?;
            Ok::<_, std::convert::Infallible>(outgoing(
                head,
                ended.load(Ordering::Acquire),
                response,
            ))
        }
    });
    let _ = tokio::time::timeout(
        options.connection_timeout,
        builder.serve_connection(TokioIo::new(socket), service),
    )
    .await;
}

/// The request body as the handler receives it; notes that it was read to its end.
struct Received {
    body: Incoming,
    ended: Arc<AtomicBool>,
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
fn outgoing(head: bool, ended: bool, response: Response<Body>) -> Response<Outgoing> {
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
    Response::from_parts(parts, Outgoing { body, length })
}

struct Outgoing {
    body: Body,
    /// the length of the body a HEAD request did not get
    length: Option<u64>,
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
