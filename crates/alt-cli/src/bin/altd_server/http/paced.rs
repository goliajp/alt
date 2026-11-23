//! A connection read and written within the time its peer is given.
//!
//! Each thing a peer does has a time of its own, counted from when the
//! server turns to it, never from when the peer could have begun:
//!
//! - the head of a request has `grace`, whole;
//! - a body has `grace` and [`Limits::per_mib`] for each MiB of it that
//!   has come, so that one of `n` MiB is over, read or given up, within
//!   `grace + n * per_mib` of the first read of it. Every byte on the
//!   wire is counted against the bytes the body may have: chunk lines,
//!   their extensions and trailers too;
//! - a response has the same for each MiB written of it.
//!
//! No single read or write waits longer than `grace`. A peer that stalls
//! or falls behind is let go of, and with it the thread that waited.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// The times peers are given.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// for the head of a request, and for anything to begin with
    pub(crate) grace: Duration,
    /// for each MiB of a body or of a response
    pub(crate) per_mib: Duration,
    /// for what a peer still sends on a connection that is being ended
    pub(crate) linger: Duration,
}

impl Limits {
    /// How long a body or a response may take in all once `moved` bytes
    /// of it have gone.
    pub(super) fn allowed(&self, moved: u64) -> Duration {
        let nanos = self.per_mib.as_nanos().saturating_mul(u128::from(moved)) >> 20;
        let earned = Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX));
        self.grace.saturating_add(earned)
    }
}

/// How long a peer has had for what it is doing now, and what it moved.
struct Pace {
    since: Instant,
    moved: u64,
    /// what is moved buys no time: the head of a request
    flat: bool,
}

impl Pace {
    fn new(flat: bool) -> Self {
        Pace {
            since: Instant::now(),
            moved: 0,
            flat,
        }
    }

    /// The longest the next read or write may wait.
    fn wait(&self, limits: &Limits) -> io::Result<Duration> {
        let allowed = limits.allowed(if self.flat { 0 } else { self.moved });
        match allowed.checked_sub(self.since.elapsed()) {
            Some(left) if !left.is_zero() => Ok(left.min(limits.grace)),
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
    limits: Limits,
    pace: Pace,
    /// the bytes that may still be taken of the body being read
    budget: u64,
}

impl Source {
    pub(super) fn new(stream: TcpStream, limits: Limits) -> Self {
        Source {
            stream,
            held: Vec::new(),
            at: 0,
            limits,
            pace: Pace::new(true),
            budget: u64::MAX,
        }
    }

    /// What is read from here on is the head of a request.
    pub(super) fn begin_head(&mut self) {
        self.pace = Pace::new(true);
        self.budget = u64::MAX;
    }

    /// What is read from here on is a body, of `most` bytes on the wire
    /// at most.
    pub(super) fn begin_body(&mut self, most: u64) {
        self.pace = Pace::new(false);
        self.budget = most;
    }

    /// Takes `count` bytes off what the body may still have.
    fn spend(&mut self, count: usize) -> io::Result<()> {
        let left = self.budget.checked_sub(count as u64);
        self.budget = left.ok_or(io::ErrorKind::FileTooLarge)?;
        Ok(())
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
        let wait = self.pace.wait(&self.limits)?;
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
        self.spend(1)?;
        self.at += 1;
        Ok(self.held[self.at - 1])
    }

    /// Sends `bytes`, each write within the time the peer has to take it.
    pub(super) fn send(&mut self, sent: &mut Sent, bytes: &[u8]) -> io::Result<()> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let wait = sent.0.wait(&self.limits)?;
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
    /// instead of being answered with a reset. The thread of the
    /// connection does this, as the worker processes of nginx do for a
    /// connection they close lingering; no worker here waits for it.
    pub(super) fn close(mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Write);
        let until = Instant::now() + self.limits.linger;
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
        self.spend(count)?;
        buf[..count].copy_from_slice(&self.ahead()[..count]);
        self.at += count;
        Ok(count)
    }
}

/// What was sent of one response, and since when.
pub(super) struct Sent(Pace);

impl Sent {
    pub(super) fn new() -> Self {
        Sent(Pace::new(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        grace: Duration::from_secs(20),
        per_mib: Duration::from_secs(8),
        linger: Duration::from_secs(2),
    };

    #[test]
    fn a_body_is_over_within_the_grace_and_the_time_of_its_mibs_whatever_its_peer_does() {
        assert_eq!(LIMITS.allowed(0), Duration::from_secs(20));
        assert_eq!(LIMITS.allowed(1 << 20), Duration::from_secs(28));
        assert_eq!(LIMITS.allowed(64 << 20), Duration::from_secs(20 + 8 * 64));
        assert_eq!(LIMITS.allowed(1 << 19), Duration::from_secs(24));
        assert!(LIMITS.allowed(u64::MAX) >= LIMITS.allowed(1 << 40));

        let mut pace = Pace::new(false);
        assert!(pace.wait(&LIMITS).unwrap() <= LIMITS.grace);
        // nothing moved in more than the grace
        pace.since = Instant::now() - Duration::from_secs(21);
        let late = pace.wait(&LIMITS).unwrap_err();
        assert_eq!(late.kind(), io::ErrorKind::TimedOut);
        // a quarter of a MiB is two seconds more
        pace.moved = 1 << 18;
        let left = pace.wait(&LIMITS).unwrap();
        assert!(left <= Duration::from_secs(1) && left > Duration::from_millis(900));
        pace.since = Instant::now() - Duration::from_secs(23);
        assert!(pace.wait(&LIMITS).is_err());
        // a GiB long ago: what it bought is not waited for in one go
        pace.moved = 1 << 30;
        assert_eq!(pace.wait(&LIMITS).unwrap(), LIMITS.grace);
        // and all of it is over when the time of a GiB is
        pace.since = Instant::now() - LIMITS.allowed(1 << 30);
        assert!(pace.wait(&LIMITS).is_err());
    }

    #[test]
    fn what_comes_of_a_head_buys_it_no_time() {
        let mut pace = Pace::new(true);
        pace.moved = 1 << 30;
        pace.since = Instant::now() - Duration::from_secs(21);
        assert!(pace.wait(&LIMITS).is_err());
    }
}
