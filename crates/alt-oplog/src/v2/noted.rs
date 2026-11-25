//! Reading `tail`, the note beside a log.
//!
//! A note says what it says of one log. One that names another log is
//! none a writer of this log left, and is taken for gone. So is one that
//! names a place past the end of the file, with one exception. A read
//! without the lock may have met a cut between its note and the file
//! made shorter, so the note is read again under the lock, which a cut
//! holds from its first step to its last: one that still says the log
//! is synced past the end of the file speaks of a log that something
//! other than a cut made shorter, and is taken at its word. Whoever
//! holds the log to that boundary is told that synced ops are lost. One
//! written for this very log earlier in the boot and put back since is
//! not told apart: nothing here makes one.

use std::fs::File;
use std::io::ErrorKind;
use std::os::unix::fs::FileExt;

use crate::v2::tail::{Held, ID, LEN, MAGIC, Noted, Place, SUMMED, Tail};

impl Tail {
    /// What the note beside the log `of` says, as the file is at this
    /// moment: the note itself where it reads, is of this log and boot,
    /// and names a place past the end of the file.
    fn noted_now(of: &Place) -> Result<Noted, Tail> {
        let Some(boot) = of.boot else {
            return Ok(Noted::Nothing);
        };
        crate::v2::note_read(of.dir);
        let file = match File::open(of.dir.join("tail")) {
            Ok(file) => file,
            Err(none) if none.kind() == ErrorKind::NotFound => return Ok(Noted::Gone),
            Err(_) => return Ok(Noted::Unread),
        };
        let mut bytes = [0u8; LEN];
        if file.read_exact_at(&mut bytes, 0).is_err() {
            return Ok(Noted::Unread);
        }
        let sum = alt_hash::blake3::hash(&bytes[..SUMMED]);
        if bytes[..4] != MAGIC || sum.as_bytes()[..8] != bytes[SUMMED..] || bytes[4] > 1 {
            return Ok(Noted::Unread);
        }
        if of.id != bytes[ID..ID + 16] {
            return Ok(Noted::Gone);
        }
        if boot != bytes[8..24] {
            return Ok(Noted::Nothing);
        }
        let word = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        let tail = Tail {
            pending: bytes[4] & 1 != 0,
            generation: u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            closes: bytes[28..32].try_into().unwrap(),
            synced: word(32),
            judged: word(40),
            hidden: word(48),
            ends: word(56),
        };
        // the file is looked at after the note: a writer makes the log
        // longer before it notes a place in it, and a cut notes the log
        // as it is once cut before it makes the file shorter
        match of.log.metadata() {
            Ok(log) if tail.synced.max(tail.judged).max(tail.ends) <= log.len() => {
                Ok(Noted::Says(tail))
            }
            Ok(_) => Err(tail),
            Err(_) => Ok(Noted::Unread),
        }
    }

    /// What a note says that names a place past the end of the file, to
    /// whoever holds the lock or found nobody to hold it: no cut is
    /// between its note and the file made shorter. One that says the log
    /// is synced past its end is of this log, which lost what it says
    /// was synced. Any other is none a writer of this log left.
    fn past_the_end(of: &Place, tail: Tail) -> Noted {
        match of.log.metadata() {
            Ok(log) if tail.synced > log.len() => Noted::Says(tail),
            Ok(_) => Noted::Gone,
            Err(_) => Noted::Unread,
        }
    }

    /// What the note beside the log `of` says to whoever holds the lock:
    /// nobody is writing it.
    pub fn noted_held(of: &Place, _held: &Held) -> Noted {
        Self::noted_now(of).unwrap_or_else(|tail| Self::past_the_end(of, tail))
    }

    /// What the note beside the log `of` says to whoever holds no lock.
    /// One that does not read is read again under the lock; the caller
    /// does not hold it.
    pub fn noted(of: &Place) -> Noted {
        let noted = Self::noted_now(of);
        if !matches!(noted, Ok(Noted::Unread) | Err(_)) {
            return noted.unwrap_or(Noted::Unread);
        }
        // without the file nobody was writing the note or cutting the log
        match (Held::after_its_holder(of.dir), noted) {
            (Ok(Some(held)), _) => Self::noted_held(of, &held),
            (Ok(None), Err(tail)) => Self::past_the_end(of, tail),
            _ => Noted::Unread,
        }
    }

    /// What the note beside the log `of` says, when it was written for
    /// that log in the running boot and reads, to whoever holds no lock.
    pub fn read(of: &Place) -> Option<Tail> {
        Self::noted(of).says()
    }
}
