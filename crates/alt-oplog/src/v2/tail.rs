//! `tail`: what the running boot knows of the log that the log does not
//! say itself yet.
//!
//! ```text
//! [0..4)   "ALTT"            [8..24)  the boot it was written in
//! [4]      bit 0, pending: among the frames from `judged` on are some
//!          another boot left that nobody vouched for
//! [24..28) the generation of the log it speaks of
//! [28..32) the four bytes of the log before `synced`
//! [32..40) synced: up to here the log was fsynced, and what its frames
//!          name with it
//! [40..48) judged: a frame that starts before here needs no vouching for
//! [48..56) hidden: when the file is this long, the log ends at `ends`
//!          for whoever opens it; zero for none
//! [56..64) ends
//! [64..72) checksum = blake3(the 64 bytes before it)[..8]
//! ```
//!
//! It is never fsynced, and nothing that is right about the log rests on
//! it alone: with the file gone at any moment, or never read, a handle
//! reads more of the log, looks at more ops and syncs more, and takes
//! nothing for looked at or synced that it would not have with it. One
//! of another boot says nothing, and the next command reads more of the
//! log instead. Whoever writes it holds `lock`,
//! as does whoever fsyncs the log to move `synced` and whoever cuts the
//! log.
//!
//! It is written in place, in one write, and read without the lock. A
//! read that finds it not to read may have met the write halfway: it
//! takes the lock, which the writer holds, and reads once more. One that
//! does not read then either was not left so by a writer of this build.
//! It may have said that ops are left to be looked at, and is taken to
//! have: an open judges every op past where the log says it is synced,
//! and whoever changes the note keeps that in it. So is one that is not
//! there in a boot whose writers leave one before they leave a
//! checkpoint: it was there and is gone. Whoever reads the note and has
//! nothing to add to it does not write it: what is written back of a
//! note that did not read would read, and say less than the one lost.
//!
//! A writer changes it with the log locked and read to its end, and what
//! it notes stands. A reader holds no lock on the log, which may have
//! grown or been cut since it read: it notes its look only where nothing
//! there says more than it knows.
//!
//! What it says, it says of the log of one generation. A cut is told in
//! the log (`mark`), and writes the note anew, of the log as it is once
//! cut, before it lays its checkpoint: the checkpoint is of this boot,
//! and whoever begins at it finds noted what is left to look at before
//! it. Until the cut is made, and for good when it never is, the note
//! speaks of another log than the one in the file. Whoever reads that
//! one takes ops for left to be looked at, and the length of a file for
//! telling nothing, as a file made shorter may grow back to it.

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::lock::{lock_exclusive, unlock};

const LEN: usize = 72;
const SUMMED: usize = LEN - 8;
const MAGIC: [u8; 4] = *b"ALTT";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Tail {
    pub pending: bool,
    pub generation: u32,
    pub closes: [u8; 4],
    pub synced: u64,
    pub judged: u64,
    pub hidden: u64,
    pub ends: u64,
}

/// What `tail` says to whoever reads it in a boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Noted {
    /// it is of another boot, or no boot is told to write one in
    Nothing,
    /// there is none
    Gone,
    /// it is there and does not read
    Unread,
    Says(Tail),
}

impl Noted {
    pub fn says(self) -> Option<Tail> {
        match self {
            Noted::Says(tail) => Some(tail),
            Noted::Nothing | Noted::Gone | Noted::Unread => None,
        }
    }

    /// What whoever changes `tail` begins with: where it does not read
    /// or is gone, that ops may be left to be looked at from the first
    /// frame on.
    pub fn to_change(self) -> Tail {
        match self {
            Noted::Says(tail) => tail,
            Noted::Nothing => Tail::default(),
            Noted::Gone | Noted::Unread => Tail {
                pending: true,
                ..Tail::default()
            },
        }
    }
}

/// `lock` beside the log, held by one at a time of those that fsync the
/// log to move the boundary, cut the log or write `tail`.
pub(crate) struct Held(File);

impl Held {
    pub fn take(dir: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))?;
        lock_exclusive(&file)?;
        Ok(Held(file))
    }
}

impl Held {
    /// Waits for whoever holds `lock` in `dir` and takes it, where the
    /// file is there: `None` where it is not, and nobody holds it then.
    /// For a reader, which makes nothing beside a log.
    pub fn after_its_holder(dir: &Path) -> std::io::Result<Option<Self>> {
        match OpenOptions::new().write(true).open(dir.join("lock")) {
            Ok(file) => {
                lock_exclusive(&file)?;
                Ok(Some(Held(file)))
            }
            Err(none) if none.kind() == ErrorKind::NotFound => Ok(None),
            Err(other) => Err(other),
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let _ = unlock(&self.0);
    }
}

impl Tail {
    /// Takes the log for synced up to `synced`, the four bytes before
    /// which are `closes` where the caller has them.
    pub fn raise(&mut self, synced: u64, closes: Option<[u8; 4]>) {
        if synced > self.synced {
            (self.synced, self.closes) = (synced, closes.unwrap_or_default());
        }
    }

    /// Whether a file `len` bytes long is, to whoever reads it without
    /// cutting it, a log up to `ends` and no further, when the log in it
    /// is of `generation`: a reader said so of the log it read. A file
    /// that a cut made shorter and that grew back to the length is of a
    /// later generation.
    pub fn hides(&self, len: u64, generation: u32) -> bool {
        self.hidden != 0 && self.hidden == len && self.generation == generation
    }

    /// Speaks of the log of `generation` as a writer has it that holds
    /// its lock and has read it to its end: what a reader noted of where
    /// the log ends holds no longer.
    pub fn stands(&mut self, generation: u32) {
        (self.generation, self.hidden, self.ends) = (generation, 0, 0);
    }

    /// What `tail` in `dir` says in `boot`, as the file is at this moment.
    fn noted_now(dir: &Path, boot: Option<[u8; 16]>) -> Noted {
        let Some(boot) = boot else {
            return Noted::Nothing;
        };
        let file = match File::open(dir.join("tail")) {
            Ok(file) => file,
            Err(none) if none.kind() == ErrorKind::NotFound => return Noted::Gone,
            Err(_) => return Noted::Unread,
        };
        let mut bytes = [0u8; LEN];
        if file.read_exact_at(&mut bytes, 0).is_err() {
            return Noted::Unread;
        }
        let sum = alt_hash::blake3::hash(&bytes[..SUMMED]);
        if bytes[..4] != MAGIC || sum.as_bytes()[..8] != bytes[SUMMED..] || bytes[4] > 1 {
            return Noted::Unread;
        }
        if boot != bytes[8..24] {
            return Noted::Nothing;
        }
        let word = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        Noted::Says(Tail {
            pending: bytes[4] & 1 != 0,
            generation: u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            closes: bytes[28..32].try_into().unwrap(),
            synced: word(32),
            judged: word(40),
            hidden: word(48),
            ends: word(56),
        })
    }

    /// What `tail` in `dir` says in `boot` to whoever holds the lock:
    /// nobody is writing it.
    pub fn noted_held(dir: &Path, boot: Option<[u8; 16]>, _held: &Held) -> Noted {
        Self::noted_now(dir, boot)
    }

    /// What `tail` in `dir` says in `boot` to whoever holds no lock. One
    /// that does not read is read again under the lock; the caller does
    /// not hold it.
    pub fn noted(dir: &Path, boot: Option<[u8; 16]>) -> Noted {
        match Self::noted_now(dir, boot) {
            // without the file nobody was writing the note
            Noted::Unread => match Held::after_its_holder(dir) {
                Ok(Some(held)) => Self::noted_held(dir, boot, &held),
                Ok(None) | Err(_) => Noted::Unread,
            },
            noted => noted,
        }
    }

    /// What `tail` in `dir` says, when it was written in `boot` and
    /// reads, to whoever holds no lock.
    pub fn read(dir: &Path, boot: Option<[u8; 16]>) -> Option<Tail> {
        Self::noted(dir, boot).says()
    }

    /// Writes `tail` in `dir` as of `boot`; the caller holds the lock.
    /// Without a boot to name nothing is written.
    pub fn write(&self, dir: &Path, boot: Option<[u8; 16]>, _held: &Held) -> std::io::Result<()> {
        let Some(boot) = boot else {
            return Ok(());
        };
        let mut bytes = [0u8; LEN];
        bytes[..4].copy_from_slice(&MAGIC);
        bytes[4] = self.pending as u8;
        bytes[8..24].copy_from_slice(&boot);
        bytes[24..28].copy_from_slice(&self.generation.to_le_bytes());
        bytes[28..32].copy_from_slice(&self.closes);
        bytes[32..40].copy_from_slice(&self.synced.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.judged.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.hidden.to_le_bytes());
        bytes[56..64].copy_from_slice(&self.ends.to_le_bytes());
        let sum = alt_hash::blake3::hash(&bytes[..SUMMED]);
        bytes[SUMMED..].copy_from_slice(&sum.as_bytes()[..8]);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("tail"))?;
        file.write_all_at(&bytes, 0)
    }

    /// Changes `tail` under the lock: `change` is given what it says for
    /// this boot, or one that says nothing. What `change` leaves as it
    /// was is not written.
    pub fn update(
        dir: &Path,
        boot: Option<[u8; 16]>,
        change: impl FnOnce(&mut Tail),
    ) -> std::io::Result<()> {
        let held = Held::take(dir)?;
        Self::update_held(dir, boot, &held, change)
    }

    /// [`Self::update`] for whoever holds the lock already.
    pub fn update_held(
        dir: &Path,
        boot: Option<[u8; 16]>,
        held: &Held,
        change: impl FnOnce(&mut Tail),
    ) -> std::io::Result<()> {
        let noted = Tail::noted_held(dir, boot, held).to_change();
        let mut tail = noted;
        change(&mut tail);
        match tail == noted {
            true => Ok(()),
            false => tail.write(dir, boot, held),
        }
    }
}
