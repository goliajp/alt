//! A connection read and written within the time its peer is given.
//!
//! A peer has `grace` to send or take anything at all, and from then on
//! has to keep up an average of [`RATE`] bytes a second: what it has moved
//! so far buys it the time to move more. One that stalls, or that moves a
//! byte now and then, is let go of, and with it the thread that waited.
//! The numbers are those Apache's `mod_reqtimeout` starts from.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// the bytes a second a peer has to average
const RATE: u64 = 500;

/// How long a peer has had for what it is doing now, and what it moved.
struct Pace {
    since: Instant,
    moved: u64,
}

impl Pace {
    fn new() -> Self {
        Pace {
            since: Instant::now(),
            moved: 0,
        }
    }

    /// The longest the next read or write may wait.
    fn wait(&self, grace: Duration) -> io::Result<Duration> {
        let earned = grace + Duration::from_millis(self.moved.saturating_mul(1000) / RATE);
        match earned.checked_sub(self.since.elapsed()) {
            Some(left) if !left.is_zero() => Ok(left.min(grace)),
            _ => Err(io::ErrorKind::TimedOut.into()),
        }
    }
}

fn timed_out(error: io::Error) -> io::Error {
    match error.kind() {
        io::ErrorKind::WouldBlock => io::ErrorKind::TimedOut.into(),
        _ => error,
    }
}

/// The bytes a peer sends: those read ahead of where the request is, then
/// the connection.
pub(crate) struct Source {
    stream: TcpStream,
    held: Vec<u8>,
    at: usize,
    grace: Duration,
    pace: Pace,
}

impl Source {
    pub(super) fn new(stream: TcpStream, grace: Duration) -> Self {
        Source {
            stream,
            held: Vec::new(),
            at: 0,
            grace,
            pace: Pace::new(),
        }
    }

    /// What is read from here on is a new thing for the peer to send.
    pub(super) fn begin(&mut self) {
        self.pace = Pace::new();
    }

    /// The bytes read and not yet taken.
    pub(super) fn ahead(&self) -> &[u8] {
        &self.held[self.at..]
    }

    pub(super) fn take(&mut self, count: usize) {
        self.at += count;
    }

    /// Reads more of what the peer sends behind what is held. The count,
    /// zero where the peer sends no more.
    pub(super) fn more(&mut self) -> io::Result<usize> {
        if self.at > 0 {
            self.held.drain(..self.at);
            self.at = 0;
        }
        let wait = self.pace.wait(self.grace)?;
        self.stream.set_read_timeout(Some(wait))?;
        let had = self.held.len();
        self.held.resize(had + 16 * 1024, 0);
        let read = self.stream.read(&mut self.held[had..]);
        self.held.truncate(had + *read.as_ref().unwrap_or(&0));
        let read = read.map_err(timed_out)?;
        self.pace.moved += read as u64;
        Ok(read)
    }

    /// One byte, or the end of what the peer sends as an error.
    pub(super) fn byte(&mut self) -> io::Result<u8> {
        if self.ahead().is_empty() && self.more()? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.at += 1;
        Ok(self.held[self.at - 1])
    }

    /// Sends `bytes`, each write within the time the peer has to take it.
    pub(super) fn send(&mut self, sent: &mut Sent, bytes: &[u8]) -> io::Result<()> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let wait = sent.0.wait(self.grace)?;
            self.stream.set_write_timeout(Some(wait))?;
            let wrote = self.stream.write(rest).map_err(timed_out)?;
            if wrote == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            sent.0.moved += wrote as u64;
            rest = &rest[wrote..];
        }
        Ok(())
    }

    /// Ends the connection so that the peer reads what was sent: nothing
    /// more is sent, and what the peer still sends is taken for a moment
    /// instead of being answered with a reset.
    pub(super) fn close(mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Write);
        let until = Instant::now() + Duration::from_secs(2).min(self.grace);
        let mut taken = [0u8; 16 * 1024];
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if left.is_zero() || self.stream.set_read_timeout(Some(left)).is_err() {
                break;
            }
            match self.stream.read(&mut taken) {
                Ok(read) if read > 0 => {}
                _ => break,
            }
        }
    }
}

impl Read for Source {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.ahead().is_empty() && self.more()? == 0 {
            return Ok(0);
        }
        let count = buf.len().min(self.ahead().len());
        buf[..count].copy_from_slice(&self.ahead()[..count]);
        self.at += count;
        Ok(count)
    }
}

/// What was sent of one response, and since when.
pub(super) struct Sent(Pace);

impl Sent {
    pub(super) fn new() -> Self {
        Sent(Pace::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_peer_moved_buys_it_time_at_the_rate_and_no_wait_is_longer_than_the_grace() {
        let grace = Duration::from_secs(20);
        let mut pace = Pace::new();
        assert!(pace.wait(grace).unwrap() <= grace);
        // nothing moved in more than the grace
        pace.since = Instant::now() - Duration::from_secs(21);
        assert_eq!(
            pace.wait(grace).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        // 1000 bytes are two seconds more
        pace.moved = 1000;
        let left = pace.wait(grace).unwrap();
        assert!(left <= Duration::from_secs(1) && left > Duration::from_millis(900));
        pace.since = Instant::now() - Duration::from_secs(23);
        assert!(pace.wait(grace).is_err());
        // a megabyte long ago: what it bought is not waited for in one go
        pace.moved = 1 << 20;
        assert_eq!(pace.wait(grace).unwrap(), grace);
    }
}
