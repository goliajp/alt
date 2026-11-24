//! Reading what other writers appended to a version 2 log since this
//! handle last read it.

use std::io::ErrorKind;

use crate::lock::{lock_shared, unlock};
use crate::v2::frame::{KIND_OP, View};
use crate::v2::log::Log;
use crate::v2::mark::Mark;
use crate::v2::open::op_of;
use crate::v2::salvage::{Salvaged, salvage};
use crate::v2::scan::Stop;
use crate::v2::state::State;
use crate::v2::stream::walk_file;
use crate::v2::tail::{Held, Tail};
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

    /// From which byte on frames this handle holds are cut off the log,
    /// or told in its header as being cut off it: `None` where none are.
    /// What a handle that holds no lock on the log does not find from
    /// there on where those frames say it is, is no damage: once the
    /// header tells it, a cut writes over them before it makes the file
    /// shorter. What does not read before that place no cut explains.
    /// With the last frame held gone, cuts were made that the header
    /// tells only the last of: the place is the first byte. The header
    /// is read first: a cut that is told there stays told until it is
    /// made, and one that is made between the two looks has taken the
    /// last frame away.
    pub(super) fn cut_from(&self) -> Result<Option<u64>, OpLogError> {
        let told = Mark::read_unlocked(&self.file)?;
        crate::v2::cut_asked();
        if !self.holds_head(self.file.metadata()?.len())? {
            return Ok(Some(0));
        }
        let begun = told.begun_at(self.state.generation, self.synced);
        Ok(begun.filter(|at| *at < self.state.end))
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
        let synced = noted.map_or(0, |tail| tail.synced_of(state.generation));
        let claim = found.as_ref().map_or(0, |found| found.claim);
        if found.is_some() && state.end < synced.max(self.synced).max(state.synced).max(claim) {
            return Err(OpLogError::Format("oplog record corrupt"));
        }
        if state.end > self.state.end {
            let closes = self.source().at(state.end - 4, 4)?;
            self.closes.copy_from_slice(&closes);
        }
        if state.generation > self.generation_known {
            // what follows a cut made since was appended after it
            self.left_to = self.left_to.min(state.cut);
            self.generation_known = state.generation;
        }
        self.state = state;
        if let Some(full) = self.full.get_mut() {
            full.extend_from_slice(&added);
        }
        self.ops.extend(added);
        Ok(found)
    }

    /// Where the log ends for a reader in a file of `size` bytes, and
    /// otherwise what it read to find that it ends nowhere short of
    /// that. It ends where a reader noted in `tail` that it does, of the
    /// log of the generation this handle holds in a file this long. And
    /// it ends where its header tells of a cut that the frames past the
    /// ones held do not hold: when the cut was made, its checkpoint is
    /// among them, and with it the log is of the generation told, in a
    /// file of any length.
    fn hidden_from(
        &self,
        (noted, told): (Option<Tail>, Mark),
        size: u64,
    ) -> Result<(Option<u64>, Option<Past>), OpLogError> {
        let (generation, end) = (self.state.generation, self.state.end);
        let hides = |tail: &Tail| tail.hides(size, generation) && tail.ends >= end;
        let noted = noted.filter(hides).map(|tail| tail.ends);
        let begun = told.begun_at(generation, self.synced);
        let Some(begun) = begun.filter(|at| *at < size) else {
            return Ok((noted, None));
        };
        let past = self.read_past(noted.unwrap_or(size))?;
        match past.0.generation >= told.generation {
            true => Ok((noted, Some(past))),
            false => Ok((Some(noted.map_or(begun, |ends| ends.min(begun))), None)),
        }
    }

    /// With the log read to its end under the exclusive lock by a
    /// handle that may write it (`writes`): fsyncs it where it ends
    /// with the checkpoint of a cut that nothing tells is on disk. Whoever cut the log may have died before it fsynced
    /// that, and nothing is written after such a checkpoint. A loss of
    /// power could take a part of it and leave what follows: the cut is
    /// then made again, of the generation the lost checkpoint was of,
    /// and frames of that generation which were cut or lost before
    /// would follow the new checkpoint as if appended after it.
    fn cut_fsynced(&mut self, writes: bool) -> Result<(), OpLogError> {
        let (cut, end) = (self.state.cut, self.state.end);
        let on_disk = self.cut_on_disk == end || self.synced >= end;
        if !writes || cut == 0 || self.state.last != cut || on_disk {
            return Ok(());
        }
        crate::v2::fsync(&self.file)?;
        self.cut_on_disk = end;
        Ok(())
    }

    /// Takes in the frames past the ones held, under a lock on the log.
    /// What follows the last of them that is no frame is cut off the file
    /// when `cut`, under the exclusive lock, and stays in it otherwise.
    fn read_on(&mut self, cut: bool) -> Result<(), OpLogError> {
        let mut size = self.file.metadata()?.len();
        let told = self.told()?;
        let noted = self.tail();
        // a cut that was begun leaves the file as long as it was at first
        let cut_away = told.cuts(self.state.generation, self.synced, self.state.end);
        if cut_away || !self.holds_head(size)? {
            return Err(OpLogError::CutBack);
        }
        // to a reader the log ends where `tail` says it does in a file of
        // this length, or where a cut of it was begun and not made
        let end = self.state.end;
        let (ends, ahead) = match cut {
            true => (None, None),
            false => self.hidden_from((noted, told), size)?,
        };
        self.hidden = ends.map(|_| size);
        size = ends.unwrap_or(size);
        let found = match ahead {
            Some(past) => self.take_in(past, noted)?,
            None if size > end => self.take_in(self.read_past(size)?, noted)?,
            None => None,
        };
        self.cut_fsynced(cut)?;
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
        } else if cuts && told.begun(self.state.generation) {
            // the log was read to its end, and the cut never came to it;
            // whoever reads what the header tells under `lock` alone
            // does not meet this write halfway
            let _held = Held::take(&self.dir)?;
            crate::v2::cut_taken_back(&self.dir);
            Mark::default().write(&self.file)?;
        }
        // a frame of this handle past where the log ended for it is
        // found now, or is gone
        if self.hidden.is_none() && self.account.lock().unwrap().unsure {
            self.owed_looked()?;
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "hooks"))]
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
