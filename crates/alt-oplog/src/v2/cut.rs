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

use crate::v2::cut_reached;
use crate::v2::frame::{Cut, UNSYNCED};
use crate::v2::log::Log;
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
            // what a reader hid from this handle is still in the file,
            // and is another boot's when the handle reads on
            let all = self.hidden.is_none();
            self.judged.set(if all { u64::MAX } else { self.state.end });
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
        self.cut_at(cut.at, discarded, true)?;
        Ok(Some(cut))
    }

    /// Cuts off what follows the last frame held, which is no frame.
    /// Nobody looked at the ops before it for that.
    pub(super) fn cut_end(&mut self, discarded: Discarded) -> Result<(), OpLogError> {
        self.cut_at(self.state.end, discarded, false)
    }

    /// What `tail` says after a cut at `at` of the ops another boot left:
    /// whether some are still to be looked at, and from where. `noted` is
    /// what it said of the log before the cut. A cut that followed a look
    /// at every op (`looked`) leaves none. Any other says no more than was
    /// known before it: the checkpoint it writes is of this boot, and tells
    /// nothing of the frames before it.
    fn left_after(&self, noted: Option<Tail>, at: u64, looked: bool) -> (bool, u64) {
        let judged = self.judged.get().min(at);
        let noted = noted.filter(|tail| tail.pending && tail.tells(self.state.generation));
        match (looked, noted) {
            (true, _) => (false, at),
            (false, Some(tail)) => (true, tail.judged.max(judged).min(at)),
            (false, None) => (judged < at, judged),
        }
    }

    /// Says where the log ends what `discarded` holds was cut, in a
    /// checkpoint of `generation` that is synced, and has the file end
    /// with it.
    fn lay_cut(&mut self, discarded: &Discarded, generation: u32) -> Result<(), OpLogError> {
        let cut = Cut {
            bytes: discarded.to - discarded.from,
            ops: discarded.ops.len() as u32,
            after: self.state.cut,
            generation: self.state.generation,
        };
        let mut state = self.state.clone();
        state.generation = generation;
        let bytes = self.composer().checkpoint(&mut state, Some(cut))?;
        self.put(&bytes, state)?;
        cut_reached(2)?;
        self.file.sync_all()?;
        cut_reached(3)?;
        self.file.set_len(self.state.end)?;
        cut_reached(4)?;
        self.file.sync_all()?;
        cut_reached(5)?;
        Ok(())
    }

    /// Cuts the log at `at`, where a frame this handle holds starts or
    /// the last one ends, under the exclusive lock of the log. Nothing is
    /// cut that was recorded as synced. `looked` says that every op
    /// before `at` was vouched for.
    ///
    /// The order holds through a power cut and through the death of this
    /// process alike. `tail` first says that a file as long as this one
    /// is a log up to `at`: the frames past it stay readable until the
    /// file is made shorter, and are no log from the next step on. Then
    /// the log says what is cut, in a checkpoint under the next
    /// generation written over it and synced. Only then is the file made
    /// to end with that checkpoint.
    fn cut_at(&mut self, at: u64, discarded: Discarded, looked: bool) -> Result<(), OpLogError> {
        self.may_write()?;
        // neither a frame that must be known is cut nor the log after one
        self.may_append()?;
        let held = Held::take(&self.dir)?;
        let noted = self.tail();
        let synced = noted.map_or(0, |tail| tail.synced).max(self.synced);
        if at < synced {
            return Err(OpLogError::Lost { synced, holds: at });
        }
        let (pending, judged) = self.left_after(noted, at, looked);
        // above the generation of every frame that reads in the file,
        // also past `at`. A cut made again after the power went while its
        // checkpoint was written takes the generation that one was of:
        // nothing else was written under it, a cut syncs its checkpoint
        // before a frame is appended after
        let cut_before = OpLogError::Format("the oplog was cut as often as it can count");
        let generation = self.state.generation.checked_add(1).ok_or(cut_before)?;
        if at != self.state.end {
            self.read_to(at)?;
            self.may_append()?;
        }
        let mut tail = Tail {
            // the ops from `at` on are there until the checkpoint lies over them
            pending: pending || looked,
            cutting: true,
            generation,
            closes: [0; 4],
            synced,
            judged: if pending { judged } else { at },
            hidden: self.file.metadata()?.len(),
            ends: at,
        };
        tail.write(&self.dir, self.boot, &held)?;
        cut_reached(1)?;
        self.lay_cut(&discarded, generation)?;
        self.unsynced = false;
        tail.stands(generation);
        // with nothing left to look at, the checkpoint is covered too
        (tail.pending, tail.judged) = (pending, if pending { judged } else { self.state.end });
        tail.write(&self.dir, self.boot, &held)?;
        if looked {
            self.judged.set(u64::MAX);
        }
        self.hidden = None;
        self.discarded.push(discarded);
        Ok(())
    }
}
