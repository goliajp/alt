//! A fixed-length file response ends with its final data frame. Hyper need not
//! poll again after Content-Length bytes, so an unknown-length ReaderStream
//! alone cannot report that terminal state to response observers.
use bytes::Bytes;
use futures_util::Stream;
use hyper::body::{Body, Frame, SizeHint};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll, ready},
};
use tokio::io::{AsyncRead, AsyncReadExt, Take};
use tokio_util::io::ReaderStream;

pub(super) struct FileBody<R> {
    stream: ReaderStream<Take<R>>,
    remaining: u64,
}

impl<R: AsyncRead> FileBody<R> {
    pub(super) fn new(reader: R, bytes: u64) -> Self {
        Self {
            stream: ReaderStream::with_capacity(reader.take(bytes), 65536),
            remaining: bytes,
        }
    }
}

impl<R: AsyncRead + Unpin> Body for FileBody<R> {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        if this.remaining == 0 {
            return Poll::Ready(None);
        }
        let result = match ready!(Pin::new(&mut this.stream).poll_next(cx)) {
            Some(Ok(bytes)) => {
                // Take limits reads to the declared response extent.
                this.remaining -= bytes.len() as u64;
                Ok(Frame::data(bytes))
            }
            Some(Err(error)) => {
                this.remaining = 0;
                Err(error)
            }
            None => {
                this.remaining = 0;
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "response file ended before its declared length",
                ))
            }
        };
        Poll::Ready(Some(result))
    }

    fn is_end_stream(&self) -> bool {
        self.remaining == 0
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_server_telemetry::{Id, Method, Metrics, ResponseObservation, Route, Trace};
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn fixed_length_completion_short_file_and_cancellation_are_distinct() {
        for case in ["complete", "short", "cancel", "empty"] {
            let metrics = Metrics::new(None).unwrap();
            let observation = ResponseObservation::new(Trace::start(
                metrics.clone(),
                Id::new("test-instance").unwrap(),
                Route::GitUpload,
                Method::Post,
            ));
            let data = vec![7; 131073];
            let declared = match case {
                "short" => data.len() as u64 + 1,
                "empty" => 0,
                _ => data.len() as u64,
            };
            let mut body = observation.body(FileBody::new(&data[..], declared), 200);
            let mut yielded = 0;
            while !body.is_end_stream() {
                assert_eq!(body.size_hint().exact(), Some(declared - yielded));
                match body.frame().await.unwrap() {
                    Ok(frame) => yielded += frame.into_data().unwrap().len() as u64,
                    Err(error) => {
                        assert_eq!(case, "short");
                        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
                        break;
                    }
                }
                if case == "cancel" {
                    assert!(yielded < declared);
                    break;
                }
            }
            // No extra EOF poll: this is how HTTP/1 handles Content-Length.
            drop(body);
            let outcome = match case {
                "short" => "failed",
                "cancel" => "dropped",
                _ => "complete",
            };
            let text = String::from_utf8(metrics.encode().unwrap()).unwrap();
            let terminals: Vec<_> = text
                .lines()
                .filter(|line| line.starts_with("alt_http_responses_total{"))
                .collect();
            assert_eq!(terminals.len(), 1, "{case}: {text}");
            assert!(
                terminals[0].contains(&format!("transport=\"{outcome}\"}} 1")),
                "{case}: {text}"
            );
            assert!(text.contains(&format!(
                "alt_http_response_bytes_total{{route=\"git_upload\"}} {yielded}"
            )));
            assert!(text.contains("alt_http_inflight 0"));
        }
    }
}
