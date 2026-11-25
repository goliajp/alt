//! The socket of a connection, written to for as long as its peer takes what is
//! written.
//!
//! A response has no time in all. It is gone on with while it moves: in each
//! window of the time the peer keeps the server waiting to write, no fewer bytes
//! than a request body has to bring in one. A peer on a slow line is as slow as its
//! line and is not let go of for it; one that stops reading, or takes a byte now
//! and then, is, within two windows of when it began to. The time the server takes
//! to have the next piece is not the peer's.
//!
//! What moved is what the peer took: what was written less what the socket still
//! holds unacknowledged. A write to a socket succeeds when a good part of its
//! buffer is free again, which a slow peer of a large buffer brings about less
//! than once in a window; going by the writes that succeeded would let go of a
//! peer many times faster than it has to be.

use super::connection::Life;
use std::{
    io,
    os::fd::AsRawFd,
    pin::Pin,
    sync::{Arc, atomic::Ordering::Relaxed},
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
    /// what was written on the connection
    written: u64,
    /// what the peer had taken of it when the window began
    taken: u64,
}
impl Taken {
    pub(super) fn new(window: Duration) -> Self {
        Taken {
            window,
            waited: Duration::ZERO,
            since: None,
            written: 0,
            taken: 0,
        }
    }

    /// The window is over where the peer kept the server waiting for all of it:
    /// false where the peer took too little in it, and the next one begins
    /// otherwise. `held` tells what the socket holds unacknowledged.
    fn over(&mut self, now: Instant, held: impl FnOnce() -> io::Result<u64>) -> io::Result<bool> {
        let waited = self.waited + self.since.map_or(Duration::ZERO, |since| now - since);
        if waited < self.window {
            return Ok(true);
        }
        let taken = self.written.saturating_sub(held()?);
        let enough = taken.saturating_sub(self.taken) >= super::body::least(self.window);
        (self.waited, self.taken) = (Duration::ZERO, taken);
        self.since = self.since.map(|_| now);
        Ok(enough)
    }

    /// The socket takes nothing now. When to look again, or nothing where the
    /// peer took too little in the window that just ended.
    pub(super) fn waiting(
        &mut self,
        now: Instant,
        held: impl FnOnce() -> io::Result<u64>,
    ) -> io::Result<Option<Instant>> {
        self.since.get_or_insert(now);
        if !self.over(now, held)? {
            return Ok(None);
        }
        let waited = self.waited + (now - self.since.expect("waiting"));
        Ok(Some(now + (self.window - waited)))
    }

    /// The socket took `count` bytes.
    pub(super) fn wrote(&mut self, now: Instant, count: usize) {
        if let Some(since) = self.since.take() {
            self.waited += now - since;
        }
        self.written += count as u64;
    }
}

pub(super) struct Paced<S> {
    io: S,
    life: Arc<Life>,
    taken: Taken,
    /// wakes the write that waits when its window is over
    look: Pin<Box<Sleep>>,
}
impl<S: AsRawFd> Paced<S> {
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
                self.life.taken.store(false, Relaxed);
                self.life.moved();
                Poll::Ready(Ok(count))
            }
            Poll::Pending => {
                let held = || alt_os::socket::unacknowledged(&self.io);
                let look = match self.taken.waiting(now, held) {
                    Ok(look) => look,
                    Err(error) => return Poll::Ready(Err(error)),
                };
                let Some(look) = look else {
                    let slow = io::Error::new(io::ErrorKind::TimedOut, "response read too slowly");
                    return Poll::Ready(Err(slow));
                };
                // nor while its peer takes what is sent as fast as it has to
                self.life.taken.store(true, Relaxed);
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
impl<S: AsyncWrite + AsRawFd + Unpin> AsyncWrite for Paced<S> {
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

    const WINDOW: Duration = Duration::from_secs(30);
    const SECOND: Duration = Duration::from_secs(1);

    /// A peer that acknowledges at once: the socket holds nothing.
    fn none() -> io::Result<u64> {
        Ok(0)
    }

    #[test]
    fn a_response_goes_on_while_its_peer_takes_it_and_only_the_time_waited_for_the_peer_counts() {
        let least = super::super::body::least(WINDOW) as usize;
        let begun = Instant::now();
        let mut taken = Taken::new(WINDOW);
        let mut now = begun;
        // a day of windows in each of which the peer takes just enough, at its end
        for _ in 0..2880 {
            assert_eq!(taken.waiting(now, none).unwrap(), Some(now + WINDOW));
            now += WINDOW;
            taken.wrote(now, least);
        }
        // the server has nothing to write for an hour: no time of the peer's
        now += 3600 * SECOND;
        assert_eq!(taken.waiting(now, none).unwrap(), Some(now + WINDOW));
        // the peer takes all but one byte of the least in a window it kept the
        // server waiting for, piece by piece
        for _ in 0..29 {
            now += SECOND;
            taken.wrote(now, least / 30);
            assert!(taken.waiting(now, none).unwrap().is_some());
        }
        now += SECOND;
        taken.wrote(now, least - 29 * (least / 30) - 1);
        assert_eq!(taken.waiting(now, none).unwrap(), None);

        // a peer that takes a great deal and then nothing: over within two windows
        let mut taken = Taken::new(WINDOW);
        let mut now = begun;
        assert_eq!(taken.waiting(now, none).unwrap(), Some(now + WINDOW));
        now += SECOND;
        taken.wrote(now, 1 << 30);
        assert_eq!(
            taken.waiting(now, none).unwrap(),
            Some(now + WINDOW - SECOND)
        );
        now += WINDOW - SECOND;
        assert_eq!(taken.waiting(now, none).unwrap(), Some(now + WINDOW));
        now += WINDOW;
        assert_eq!(taken.waiting(now, none).unwrap(), None);
    }

    /// The socket has a buffer of four megabytes and takes a write when a third of
    /// it is free. The peer reads 8 KiB a second, eight times what it has to: a
    /// write succeeds once in close to three minutes, and in the windows between
    /// none does. The response goes on for an hour of that. A peer that then
    /// reads nothing is let go of within two windows.
    #[test]
    fn a_peer_that_takes_enough_is_kept_though_no_write_succeeds_in_a_window() {
        const BUFFER: u64 = 4 << 20;
        const RATE: u64 = 8 << 10;
        let mut taken = Taken::new(WINDOW);
        let mut now = Instant::now();
        // the buffer is filled at once
        taken.wrote(now, BUFFER as usize);
        let mut held = BUFFER;
        let mut writes = 0;
        for second in 0..3600 {
            // the write waits: looked at each second, and when a window ends
            let looked = taken.waiting(now, || Ok(held)).unwrap();
            assert!(looked.is_some(), "let go of after {second} seconds");
            now += SECOND;
            held -= RATE;
            if held <= BUFFER - BUFFER / 3 {
                taken.wrote(now, (BUFFER - held) as usize);
                held = BUFFER;
                writes += 1;
            }
        }
        assert!(writes < 3600 / 170 + 1, "{writes} writes");
        // from here the peer takes nothing
        let mut windows = 0;
        while taken.waiting(now, || Ok(held)).unwrap().is_some() {
            now += WINDOW;
            windows += 1;
        }
        assert!(windows <= 2, "{windows} windows");
    }

    #[test]
    fn a_socket_tells_what_its_peer_has_not_acknowledged() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut sender = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut receiver, _) = listener.accept().unwrap();
        assert_eq!(alt_os::socket::unacknowledged(&sender).unwrap(), 0);
        // more than the two sockets hold between them: the rest is not taken
        sender.set_nonblocking(true).unwrap();
        let piece = [7u8; 64 << 10];
        let mut written = 0u64;
        while let Ok(count) = sender.write(&piece) {
            written += count as u64;
        }
        let held = alt_os::socket::unacknowledged(&sender).unwrap();
        assert!(held > 0 && held < written, "{held} of {written}");
        // read to the last byte, it is all acknowledged before long
        let mut read = 0u64;
        let mut buffer = vec![0u8; 1 << 20];
        while read < written {
            read += receiver.read(&mut buffer).unwrap() as u64;
        }
        let until = std::time::Instant::now() + Duration::from_secs(30);
        while alt_os::socket::unacknowledged(&sender).unwrap() > 0 {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
