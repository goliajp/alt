//! One accepted connection: bounded HTTP/1 framing around the router.
use super::HttpOptions;
use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request, Response, header},
};
use bytes::Bytes;
use hyper::{
    body::{Frame, Incoming, SizeHint},
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
    let service = service_fn(move |mut request: Request<Incoming>| {
        request.extensions_mut().insert(ConnectInfo(peer));
        let head = request.method() == Method::HEAD;
        let response = router.call(request);
        async move { response.await.map(|response| outgoing(head, response)) }
    });
    let _ = tokio::time::timeout(
        options.connection_timeout,
        builder.serve_connection(TokioIo::new(socket), service),
    )
    .await;
}

/// The router writes the length of every sized body into the headers and answers
/// HEAD with an empty one. The connection writes that length itself, after its own
/// headers, so only a length the handler declared stays in the header map.
fn outgoing(head: bool, response: Response<Body>) -> Response<Outgoing> {
    let (mut parts, body) = response.into_parts();
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
