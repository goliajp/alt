//! Reading what other writers appended to a version 2 log since this
//! handle last read it.

use std::io::ErrorKind;

use crate::lock::{lock_shared, unlock};
use crate::v2::frame::{KIND_OP, View};
use crate::v2::log::Log;
use crate::v2::open::op_of;
use crate::v2::salvage::{Salvaged, salvage};
use crate::v2::scan::Stop;
use crate::v2::state::State;
use crate::v2::stream::walk_file;
use crate::v2::tail::Tail;
use crate::{Discarded, Op, OpLogError};

/// The log through frames read past the ones a handle holds, their ops,
/// and what still reads after them where what follows is no frame.
type Past = (State, Vec<Op>, Option<Salvaged>);

impl Log {
    pub fn refresh(&mut self) -> Result<(), OpLogError> {
        self.locked(|log| log.catch_up())?
    }

    pub fn follow(&mut self) -> Result<(), OpLogError> {
        lock_shared(&self.file)?;
        let read = self.read_on(false);
        let _ = unlock(&self.file);
        read
    }

    /// Reads the frames other writers appended and cuts off what a
    /// crashed one left at the end, which stays in a log this build may
    /// not write or append to; the caller holds the exclusive lock.
    pub(super) fn catch_up(&mut self) -> Result<(), OpLogError> {
        self.read_on(self.writable)
    }

    /// Whether the last frame this handle holds is still where the handle
    /// has it, in a file of `size` bytes: a cut takes it away, and what is
    /// appended after reads otherwise.
    pub(super) fn holds_head(&self, size: u64) -> Result<bool, OpLogError> {
        if self.state.last == 0 {
            return Ok(true);
        }
        if size < self.state.end {
            return Ok(false);
        }
        // nor does a file made shorter since `size` was taken hold it
        match self.source().at(self.state.end - 4, 4) {
            Ok(closes) => Ok(closes == self.closes),
            Err(short) if short.kind() == ErrorKind::UnexpectedEof => Ok(false),
            Err(other) => Err(other.into()),
        }
    }

    /// Whether frames this handle holds are cut off the log, or noted in
    /// `tail` as being cut off it. What a handle that holds no lock on
    /// the log does not find where those frames say it is, is then no
    /// damage: from the note on a cut writes over them before it makes
    /// the file shorter.
    pub(super) fn cut_away(&self) -> Result<bool, OpLogError> {
        if !self.holds_head(self.file.metadata()?.len())? {
            return Ok(true);
        }
        let (generation, end) = (self.state.generation, self.state.end);
        let noted = Tail::read(&self.dir, self.boot);
        Ok(noted.is_some_and(|tail| tail.cuts(generation, end)))
    }

    /// The log through the frames that follow the ones held in a file of
    /// `size` bytes, their ops, and, where what follows them is no frame,
    /// what still reads after it.
    fn read_past(&self, size: u64) -> Result<Past, OpLogError> {
        let source = self.source();
        let mut read = |at: u64, len: usize| source.at(at, len);
        let mut state = self.state.clone();
        let mut added = Vec::new();
        let mut each = |at: u64, end: u64, view: &View| {
            if view.head.kind == KIND_OP {
                added.push(op_of(at, end, view)?);
            }
            Ok(())
        };
        let stop = walk_file(&mut read, &self.layout, &mut state, size, false, &mut each)?;
        let found = match stop {
            Stop::Unread => {
                let (end, generation) = (state.end, state.generation);
                let held = source.stretches(end, size)?;
                let within = (end, size);
                Some(salvage(&mut read, &self.layout, within, generation, &held)?)
            }
            Stop::End => None,
        };
        Ok((state, added, found))
    }

    /// Takes in the frames `past` holds, which follow the ones held, and
    /// gives what still reads after them where what follows is no frame;
    /// `noted` is what `tail` says.
    fn take_in(&mut self, past: Past, noted: Option<Tail>) -> Result<Option<Salvaged>, OpLogError> {
        let (state, added, found) = past;
        let synced = noted.map_or(0, |tail| tail.synced);
        let claim = found.as_ref().map_or(0, |found| found.claim);
        if found.is_some() && state.end < synced.max(self.synced).max(state.synced).max(claim) {
            return Err(OpLogError::Format("oplog record corrupt"));
        }
        if state.end > self.state.end {
            let closes = self.source().at(state.end - 4, 4)?;
            self.closes.copy_from_slice(&closes);
        }
        self.state = state;
        if let Some(full) = self.full.get_mut() {
            full.extend_from_slice(&added);
        }
        self.ops.extend(added);
        Ok(found)
    }

    /// Where the log ends for a reader in a file of `size` bytes whose
    /// end `tail` hides from it, and otherwise what it read to find that
    /// the note holds no longer. A reader's note is of the log of the
    /// generation this handle holds. A cut that was begun is noted of
    /// the log before it, which is what this handle holds whether or not
    /// the cut was made since: when it was, its checkpoint is among the
    /// frames that follow, and with it the log is of the generation
    /// noted, in a file of any length, also one grown back to the length
    /// the cut found.
    fn hidden_from(
        &self,
        noted: Option<Tail>,
        size: u64,
    ) -> Result<(Option<u64>, Option<Past>), OpLogError> {
        let (generation, end) = (self.state.generation, self.state.end);
        let hides = |tail: &Tail| tail.hides(size, generation) && tail.ends >= end;
        let Some(tail) = noted.filter(hides) else {
            return Ok((None, None));
        };
        if tail.cutting {
            let past = self.read_past(size)?;
            if past.0.generation >= tail.generation {
                return Ok((None, Some(past)));
            }
        }
        Ok((Some(tail.ends), None))
    }

    /// Forgets in `tail` a cut it notes as begun: for a writer that holds
    /// the lock of the log and has read it to its end, where nothing
    /// follows the last frame. Either the log is of a generation before
    /// the cut's: the cut never came to it, and its frames are all there.
    /// Or the cut was made and the note not written again: what was left
    /// to be looked at from where it cut is gone, and every frame since
    /// is of this boot.
    fn cut_over(&self) -> std::io::Result<()> {
        let (generation, end) = (self.state.generation, self.state.end);
        Tail::update(&self.dir, self.boot, |tail| {
            let made = tail.cutting && tail.generation <= generation;
            let left = tail.pending && tail.judged < tail.ends;
            tail.stands(generation);
            if made && !left {
                (tail.pending, tail.judged) = (false, end);
            }
        })
    }

    /// Takes in the frames past the ones held, under a lock on the log.
    /// What follows the last of them that is no frame is cut off the file
    /// when `cut`, under the exclusive lock, and stays in it otherwise.
    fn read_on(&mut self, cut: bool) -> Result<(), OpLogError> {
        let mut size = self.file.metadata()?.len();
        let noted = self.tail();
        // a cut that was begun leaves the file as long as it was at first
        let cut_away = |tail: Tail| tail.cuts(self.state.generation, self.state.end);
        if !self.holds_head(size)? || noted.is_some_and(cut_away) {
            return Err(OpLogError::CutBack);
        }
        // to a reader the log ends where `tail` says it does in a file of
        // this length, while that is said of the log as it is
        let end = self.state.end;
        let (ends, ahead) = match cut {
            true => (None, None),
            false => self.hidden_from(noted, size)?,
        };
        self.hidden = ends.map(|_| size);
        size = ends.unwrap_or(size);
        let found = match ahead {
            Some(past) => self.take_in(past, noted)?,
            None if size > end => self.take_in(self.read_past(size)?, noted)?,
            None => None,
        };
        // a cut is said in a frame appended where the log is cut
        let cuts = cut && self.knows_all()?;
        self.passed = self.hidden.is_some() || (!cuts && found.is_some());
        if let Some(found) = found.filter(|_| cuts) {
            let discarded = Discarded {
                from: self.state.end,
                to: size,
                unread: (size - self.state.end).saturating_sub(found.covered),
                ops: found.ops,
            };
            self.cut_end(discarded)?;
        } else if cuts && noted.is_some_and(|tail| tail.cutting) {
            self.cut_over()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::v2::log::Log;
    use crate::{Meta, Op, OpLogError};

    #[test]
    fn a_file_made_shorter_after_its_length_was_taken_does_not_hold_the_last_frame() {
        let dir = alt_os::temp::tempdir().unwrap();
        crate::v2::in_boot(Some([1; 16]));
        crate::v2::create_with(dir.path(), 12).unwrap();
        let mut log = Log::open_at(dir.path(), None).unwrap();
        let payload = |_: &[Op]| Ok::<_, OpLogError>(b"x".to_vec());
        log.append_checked("a", 1, &Meta::default(), payload)
            .unwrap();
        let len = log.file.metadata().unwrap().len();
        assert!(log.holds_head(len).unwrap());
        log.file.set_len(len - 1).unwrap();
        // the length is the one the file had before it was cut
        assert!(!log.holds_head(len).unwrap());
    }
}
