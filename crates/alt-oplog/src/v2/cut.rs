//! Frames at the end of the log that nobody vouched for, and cutting the
//! log.
//!
//! Past where the log is synced a frame may have reached the disk while
//! what its op names did not. In the boot that wrote it nothing is lost
//! yet and there is nothing to look at. After the machine went down the
//! ops there are held to what they name (`held`): a writer cuts the log
//! from the first one on that names content which is not stored, a reader
//! is told where that op starts. Whoever has looked notes it in `tail`,
//! and the next command of this boot does not look again.
//!
//! Every cut is told: the frames that read among what was cut are kept
//! for the caller ([`Discarded`]), and the log gets a checkpoint where it
//! was cut that says how many bytes went, under the next generation, so
//! that nothing written before the cut reads in a place after it.

use std::sync::OnceLock;

use crate::v2::block::FILE_HEADER_LEN;
use crate::v2::frame::{Cut, KIND_OP, UNSYNCED, View};
use crate::v2::log::Log;
use crate::v2::open::op_of;
use crate::v2::scan::Stop;
use crate::v2::state::State;
use crate::v2::stream::walk_file;
use crate::v2::tail::{Held, Tail};
use crate::{Discarded, Op, OpLogError, Unconfirmed};

impl Log {
    pub fn unsettled(&self) -> bool {
        self.hidden.is_some() || self.judged.get() < self.state.end
    }

    pub fn unsettled_past<E: From<OpLogError>>(
        &self,
        _held: impl FnMut(&Op) -> Result<bool, E>,
    ) -> Result<bool, E> {
        // a writer looks once, and notes that it did
        Ok(self.unsettled())
    }

    /// The ops among which those whose frames start at or after `from`
    /// are all found: the ones held, or all of the log where the read
    /// began after `from`.
    fn ops_past(&self, from: u64) -> Result<&[Op], OpLogError> {
        match from < self.began {
            true => self.ops(),
            false => Ok(&self.ops),
        }
    }

    /// The first op whose frame starts at or after `from` that `held`
    /// does not vouch for, told whether its writer left it unsynced, with
    /// the ops after it.
    fn first_unheld<E: From<OpLogError>>(
        &self,
        from: u64,
        mut held: impl FnMut(&Op, bool) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        let ops = self.ops_past(from)?;
        for (index, op) in ops.iter().enumerate() {
            let Some(frame) = op.frame.filter(|frame| frame.at >= from) else {
                continue;
            };
            if !held(op, frame.flags & UNSYNCED != 0)? {
                return Ok(Some(Unconfirmed {
                    at: frame.at,
                    ops: ops.len() - index,
                }));
            }
        }
        Ok(None)
    }

    pub fn unconfirmed_past<E: From<OpLogError>>(
        &self,
        held: impl FnMut(&Op, bool) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        let from = self.judged.get();
        if from >= self.state.end {
            return Ok(None);
        }
        let found = self.first_unheld(from, held)?;
        // a note that is not written costs the next command this look
        let _ = self.note_look(found.map(|found| found.at));
        if found.is_none() {
            self.judged.set(u64::MAX);
        }
        Ok(found)
    }

    pub fn settle_past<E: From<OpLogError>>(
        &mut self,
        held: impl FnMut(&Op, bool) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        self.locked(|log| log.settle_locked(held))
            .map_err(OpLogError::from)?
    }

    fn settle_locked<E: From<OpLogError>>(
        &mut self,
        held: impl FnMut(&Op, bool) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        match self.catch_up() {
            Err(OpLogError::CutBack) => {
                self.read_anew()?;
                self.catch_up()?;
            }
            caught_up => caught_up?,
        }
        self.check_synced()?;
        let from = self.judged.get();
        if from >= self.state.end {
            return Ok(None);
        }
        let Some(cut) = self.first_unheld(from, held)? else {
            self.note_settled().map_err(OpLogError::from)?;
            self.judged.set(u64::MAX);
            return Ok(None);
        };
        let discarded = Discarded {
            from: cut.at,
            to: self.state.end,
            ops: {
                let ops = self.ops_past(from)?;
                ops[ops.len() - cut.ops..].to_vec()
            },
            unread: 0,
        };
        self.cut_at(cut.at, discarded)?;
        Ok(Some(cut))
    }

    /// Cuts off what follows the last frame held, which is no frame.
    pub(super) fn cut_end(&mut self, discarded: Discarded) -> Result<(), OpLogError> {
        self.cut_at(self.state.end, discarded)
    }

    /// Cuts the log at `at`, where a frame this handle holds starts or
    /// the last one ends, under the exclusive lock of the log. The log
    /// says so before anything is gone: a checkpoint under the next
    /// generation is written over what is cut and synced, and only then
    /// is the file made to end with it. Nothing is cut that was recorded
    /// as synced.
    fn cut_at(&mut self, at: u64, discarded: Discarded) -> Result<(), OpLogError> {
        self.may_write()?;
        let held = Held::take(&self.dir)?;
        let synced = self.tail().map_or(0, |tail| tail.synced).max(self.synced);
        if at < synced {
            return Err(OpLogError::Lost { synced, holds: at });
        }
        if at != self.state.end {
            self.read_to(at)?;
        }
        let cut = Cut {
            bytes: discarded.to - discarded.from,
            ops: discarded.ops.len() as u32,
        };
        let mut state = self.state.clone();
        let cut_before = OpLogError::Format("the oplog was cut as often as it can count");
        state.generation = state.generation.checked_add(1).ok_or(cut_before)?;
        let bytes = self.composer().checkpoint(&mut state, Some(cut))?;
        self.put(&bytes, state)?;
        self.file.sync_all()?;
        self.file.set_len(self.state.end)?;
        self.file.sync_all()?;
        self.unsynced = false;
        let tail = Tail {
            pending: false,
            generation: self.state.generation,
            closes: [0; 4],
            synced,
            judged: self.state.end,
            hidden: 0,
        };
        tail.write(&self.dir, self.boot, &held)?;
        self.judged.set(u64::MAX);
        self.hidden = None;
        self.discarded.push(discarded);
        Ok(())
    }

    /// Holds the log as it stands up to `at`, where a frame of it
    /// starts, read from its first frame: the ops after are no longer
    /// held, and state derived from the ops of this handle is derived
    /// again.
    fn read_to(&mut self, at: u64) -> Result<(), OpLogError> {
        let source = self.source();
        let mut read = |at: u64, len: usize| source.at(at, len);
        let (mut state, mut ops) = (State::empty(), Vec::new());
        let mut each = |at: u64, end: u64, view: &View| {
            if view.head.kind == KIND_OP {
                ops.push(op_of(at, end, view)?);
            }
            Ok(())
        };
        let stop = walk_file(&mut read, &self.layout, &mut state, at, false, &mut each)?;
        if stop != Stop::End {
            return Err(OpLogError::Format("oplog record corrupt"));
        }
        let closes = match state.last {
            0 => vec![0; 4],
            _ => source.at(at - 4, 4)?,
        };
        self.closes.copy_from_slice(&closes);
        (self.state, self.ops, self.full) = (state, ops, OnceLock::new());
        (self.began, self.began_linked) = (FILE_HEADER_LEN as u64, 0);
        (self.base, self.recent_from, self.passed) = (None, 0, false);
        self.reads += 1;
        Ok(())
    }

    pub fn reopen(&mut self) -> Result<(), OpLogError> {
        crate::lock::lock_shared(&self.file)?;
        let read = self.read_anew();
        let _ = crate::lock::unlock(&self.file);
        read
    }

    /// Reads the log again from its first frame; the caller holds a lock
    /// on it. Whoever derived state from the ops of this handle derives
    /// it again.
    fn read_anew(&mut self) -> Result<(), OpLogError> {
        self.read_all()?;
        self.reads += 1;
        Ok(())
    }

    /// Holds every op of the log, read from its first frame.
    pub(super) fn read_all(&mut self) -> Result<(), OpLogError> {
        self.read_in(true)?;
        self.full = OnceLock::new();
        Ok(())
    }
}
