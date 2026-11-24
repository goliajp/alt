//! The socket of a connection, written to for as long as its peer takes what is
//! written.
//!
//! A response has no time in all. It is gone on with while it moves: in each
//! window of the time the peer keeps the server waiting to write, no fewer bytes
//! than a request body has to bring in one. A peer on a slow line is as slow as its
//! line and is not let go of for it; one that stops reading, or takes a byte now
//! and then, is, within two windows of when it began to. The time the server takes
//! to have the next piece is not the peer's.

use super::connection::Life;
use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    time::{Instant, Sleep},
};

/// What a peer took in the time it kept the server waiting.
pub(super) struct Taken {
    window: Duration,
    /// the time waited in the window that is running, to the last write
    waited: Duration,
    /// since when the server waits to write
    since: Option<Instant>,
    /// what was written since the window began
    moved: u64,
}
impl Taken {
    pub(super) fn new(window: Duration) -> Self {
        Taken {
            window,
            waited: Duration::ZERO,
            since: None,
            moved: 0,
        }
    }

    /// The window is over where the peer kept the server waiting for all of it:
    /// false where too little moved in it, and the next one begins otherwise.
    fn over(&mut self, now: Instant) -> bool {
        let waited = self.waited + self.since.map_or(Duration::ZERO, |since| now - since);
        if waited < self.window {
            return true;
        }
        let enough = self.moved >= super::body::least(self.window);
        (self.waited, self.moved) = (Duration::ZERO, 0);
        self.since = self.since.map(|_| now);
        enough
    }

    /// The peer takes nothing now. When to look again, or nothing where it took
    /// too little in the window that just ended.
    pub(super) fn waiting(&mut self, now: Instant) -> Option<Instant> {
        self.since.get_or_insert(now);
        if !self.over(now) {
            return None;
        }
        let waited = self.waited + (now - self.since.expect("waiting"));
        Some(now + (self.window - waited))
    }

    /// The peer took `count` bytes.
    pub(super) fn wrote(&mut self, now: Instant, count: usize) {
        if let Some(since) = self.since.take() {
            self.waited += now - since;
        }
        self.moved += count as u64;
    }
}

pub(super) struct Paced<S> {
    io: S,
    life: Arc<Life>,
    taken: Taken,
    /// wakes the write that waits when its window is over
    look: Pin<Box<Sleep>>,
}
impl<S> Paced<S> {
    pub(super) fn new(io: S, life: Arc<Life>, window: Duration) -> Self {
        Paced {
            io,
            life,
            taken: Taken::new(window),
            look: Box::pin(tokio::time::sleep(Duration::ZERO)),
        }
    }

    fn written(
        &mut self,
        cx: &mut Context<'_>,
        written: Poll<io::Result<usize>>,
    ) -> Poll<io::Result<usize>> {
        let now = Instant::now();
        match written {
            Poll::Ready(Ok(count)) => {
                self.taken.wrote(now, count);
                // a connection is not over while what is sent on it moves
                self.life.moved();
                Poll::Ready(Ok(count))
            }
            Poll::Pending => {
                let Some(look) = self.taken.waiting(now) else {
                    let slow = io::Error::new(io::ErrorKind::TimedOut, "response read too slowly");
                    return Poll::Ready(Err(slow));
                };
                self.look.as_mut().reset(look);
                // the window ends while this write waits: it is looked at then
                if self.look.as_mut().poll(cx).is_ready() {
                    cx.waker().wake_by_ref();
                }
                Poll::Pending
            }
            failed => failed,
        }
    }
}
impl<S: AsyncRead + Unpin> AsyncRead for Paced<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for Paced<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let written = Pin::new(&mut self.io).poll_write(cx, buf);
        self.written(cx, written)
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let written = Pin::new(&mut self.io).poll_write_vectored(cx, bufs);
        self.written(cx, written)
    }
    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_goes_on_while_its_peer_takes_it_and_only_the_time_waited_for_the_peer_counts() {
        let window = Duration::from_secs(30);
        let least = super::super::body::least(window) as usize;
        let second = Duration::from_secs(1);
        let begun = Instant::now();
        let mut taken = Taken::new(window);
        let mut now = begun;
        // a day of windows in each of which the peer takes just enough, at its end
        for _ in 0..2880 {
            assert_eq!(taken.waiting(now), Some(now + window));
            now += window;
            taken.wrote(now, least);
        }
        // the server has nothing to write for an hour: no time of the peer's
        now += 3600 * second;
        assert_eq!(taken.waiting(now), Some(now + window));
        // the peer takes all but one byte of the least in a window it kept the
        // server waiting for, piece by piece
        for _ in 0..29 {
            now += second;
            taken.wrote(now, least / 30);
            assert!(taken.waiting(now).is_some());
        }
        now += second;
        taken.wrote(now, least - 29 * (least / 30) - 1);
        assert_eq!(taken.waiting(now), None);

        // a peer that takes a great deal and then nothing: over within two windows
        let mut taken = Taken::new(window);
        let mut now = begun;
        assert_eq!(taken.waiting(now), Some(now + window));
        now += second;
        taken.wrote(now, 1 << 30);
        assert_eq!(taken.waiting(now), Some(now + window - second));
        now += window - second;
        assert_eq!(taken.waiting(now), Some(now + window));
        now += window;
        assert_eq!(taken.waiting(now), None);
    }
}
