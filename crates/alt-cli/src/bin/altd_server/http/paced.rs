//! A connection read and written for as long as its peer goes on.
//!
//! Each thing a peer does has a time of its own, counted from when the
//! server turns to it, never from when the peer could have begun:
//!
//! - the head of a request has `grace`, whole;
//! - a body and a response have no time in all. They are gone on with
//!   while they move: in each [`Limits::window`] that passes, no fewer
//!   than [`Limits::floor`] bytes. A peer on a slow line is as slow as its
//!   line and is not let go of for it; one that stalls, or lets through a
//!   byte now and then, is, within two windows of when it began to.
//!
//! What moves of a body is what the body has: the lines around the chunks
//! of one sent in chunks are not it, and a peer that sends much of those
//! around little else has moved little.
//!
//! What a slow peer keeps is its connection and the thread of it, never
//! a worker: see [`super`] for what bounds those.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// What peers are given.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// for the head of a request
    pub(crate) grace: Duration,
    /// the time in which a body or a response has to move
    pub(crate) window: Duration,
    /// the bytes it has to move in that time
    pub(crate) floor: u64,
    /// for what a peer still sends on a connection that is being ended
    pub(crate) linger: Duration,
    /// the connections one address may have open at once; one of this
    /// host has no limit, a proxy in front of the server being one
    pub(crate) each: usize,
    /// the bytes of bodies received and not yet let go of by their
    /// requests, all together
    pub(crate) spool: u64,
}

/// How a peer does at what it is doing now.
struct Pace {
    /// when the window began
    since: Instant,
    /// what moved in it
    moved: u64,
    /// the head of a request: all of it within the grace
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

    /// The longest the next read or write may wait: to the end of the
    /// window, where what moved in it is looked at.
    fn wait(&mut self, limits: &Limits) -> io::Result<Duration> {
        let whole = if self.flat {
            limits.grace
        } else {
            limits.window
        };
        if self.since.elapsed() >= whole {
            if self.flat || self.moved < limits.floor {
                return Err(io::ErrorKind::TimedOut.into());
            }
            *self = Pace::new(false);
        }
        Ok(whole
            .saturating_sub(self.since.elapsed())
            .max(Duration::from_millis(1)))
    }
}

/// Whether `error` is that of a read or a write that waited its time out.
fn waited_out(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
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
    /// the bytes taken of it, the lines around its chunks among them
    spent: u64,
    from: Option<std::net::IpAddr>,
}

impl Source {
    pub(super) fn new(stream: TcpStream, limits: Limits, from: Option<std::net::IpAddr>) -> Self {
        Source {
            stream,
            held: Vec::new(),
            at: 0,
            limits,
            pace: Pace::new(true),
            budget: u64::MAX,
            spent: 0,
            from,
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
        self.spent = 0;
    }
    pub(super) fn spent(&self) -> u64 {
        self.spent
    }
    pub(super) fn from(&self) -> Option<std::net::IpAddr> {
        self.from
    }

    /// Takes `count` bytes off what the body may still have.
    fn spend(&mut self, count: usize) -> io::Result<()> {
        let left = self.budget.checked_sub(count as u64);
        self.budget = left.ok_or(io::ErrorKind::FileTooLarge)?;
        self.spent += count as u64;
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
        let limits = self.limits;
        let had = self.held.len();
        // a read that waited to the end of a window is no end: what
        // moved in the window is looked at, and the next begins
        loop {
            let wait = self.pace.wait(&limits)?;
            self.stream.set_read_timeout(Some(wait))?;
            self.held.resize(had + 16 * 1024, 0);
            let read = self.stream.read(&mut self.held[had..]);
            self.held.truncate(had + *read.as_ref().unwrap_or(&0));
            match read {
                Err(error) if waited_out(&error) => {}
                read => return read,
            }
        }
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
            let wrote = match self.stream.write(rest) {
                Err(error) if waited_out(&error) => continue,
                wrote => wrote?,
            };
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
        self.pace.moved += count as u64;
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
        window: Duration::from_secs(30),
        floor: 30 << 10,
        linger: Duration::from_secs(2),
        each: 64,
        spool: 4 << 30,
    };

    #[test]
    fn a_body_goes_on_while_it_moves_and_is_over_in_the_window_in_which_it_does_not() {
        let mut pace = Pace::new(false);
        assert!(pace.wait(&LIMITS).unwrap() <= LIMITS.window);
        // a window that ends with enough moved in it begins another, and
        // that for as long as the peer keeps it up: a day of windows here
        for _ in 0..2880 {
            pace.moved = LIMITS.floor;
            pace.since = Instant::now() - LIMITS.window;
            let left = pace.wait(&LIMITS).unwrap();
            assert!(left > Duration::from_secs(29) && pace.moved == 0);
        }
        // within a window nothing is asked of what moved
        pace.since = Instant::now() - Duration::from_secs(29);
        assert!(pace.wait(&LIMITS).unwrap() <= Duration::from_secs(1));
        // one byte short at the end of one is the end
        pace.moved = LIMITS.floor - 1;
        pace.since = Instant::now() - LIMITS.window;
        assert_eq!(
            pace.wait(&LIMITS).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn what_comes_of_a_head_buys_it_no_time() {
        let mut pace = Pace::new(true);
        pace.moved = 1 << 30;
        pace.since = Instant::now() - Duration::from_secs(19);
        assert!(pace.wait(&LIMITS).unwrap() <= Duration::from_secs(1));
        pace.since = Instant::now() - Duration::from_secs(21);
        assert!(pace.wait(&LIMITS).is_err());
    }
}
