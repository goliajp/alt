use crate::{Trace, Transport};
use bytes::Buf;
use http_body::{Body, Frame, SizeHint};
use std::{
    pin::Pin,
    task::{Context, Poll},
};

/// Owns exactly one terminal transport observation. Before headers, dropping it
/// means the handler disappeared. After headers it follows the response body.
/// Complete means all frames were yielded, never proof of receipt by the peer.
pub struct ResponseObservation {
    trace: Trace,
    status: u16,
    bytes: u64,
    ended: bool,
}
impl ResponseObservation {
    pub fn new(trace: Trace) -> Self {
        Self {
            trace,
            status: 0,
            bytes: 0,
            ended: false,
        }
    }
    pub fn body<B: Body>(mut self, body: B, status: u16) -> ObservedBody<B> {
        self.status = status;
        self.trace.headers(status);
        if body.is_end_stream() {
            self.finish(Transport::Complete);
        }
        ObservedBody {
            inner: body,
            observation: self,
        }
    }
    fn finish(&mut self, outcome: Transport) {
        if !self.ended {
            self.ended = true;
            self.trace.transport(self.status, self.bytes, outcome);
        }
    }
}
impl Drop for ResponseObservation {
    fn drop(&mut self) {
        self.finish(Transport::Dropped);
    }
}

pub struct ObservedBody<B> {
    inner: B,
    observation: ResponseObservation,
}
impl<B: Body + Unpin> Body for ObservedBody<B> {
    type Data = B::Data;
    type Error = B::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let frame = Pin::new(&mut this.inner).poll_frame(cx);
        match &frame {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.observation.bytes = this
                        .observation
                        .bytes
                        .saturating_add(data.remaining() as u64);
                }
                if this.inner.is_end_stream() {
                    this.observation.finish(Transport::Complete);
                }
            }
            Poll::Ready(Some(Err(_))) => this.observation.finish(Transport::Failed),
            Poll::Ready(None) => this.observation.finish(Transport::Complete),
            Poll::Pending => (),
        }
        frame
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
