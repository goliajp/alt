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
//! that nothing written before the cut reads in a place after it. Before
//! that the header of the log tells where it is cut (`mark`): a handle
//! that holds frames past that place appends after none of them.

use crate::v2::cut_reached;
use crate::v2::frame::{Cut, UNSYNCED};
use crate::v2::log::Log;
use crate::v2::mark::Mark;
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
    pub(super) fn ops_past(&self, from: u64) -> Result<&[Op], OpLogError> {
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
        mut held: impl FnMut(&Op, bool) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        let from = self.judged.get();
        if from >= self.state.end {
            return Ok(None);
        }
        let found = self.first_unheld(from, &mut held)?;
        // a writer told the same would not cut the log there: the op is
        // followed by a frame to keep, and the log ends nowhere short
        // of that for a reader
        if let Some(cut) = found {
            let discarded = self.to_cut(from, cut)?;
            if let Some(kept) = self.kept_past(&discarded, held)? {
                return Err(OpLogError::Kept {
                    unheld: cut.at,
                    kept,
                }
                .into());
            }
        }
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

    /// What a cut at the op `cut` tells of takes off the log, where the
    /// ops from byte `from` on were looked at.
    fn to_cut(&self, from: u64, cut: Unconfirmed) -> Result<Discarded, OpLogError> {
        let ops = self.ops_past(from)?;
        Ok(Discarded {
            from: cut.at,
            to: self.state.end,
            ops: ops[ops.len() - cut.ops..].to_vec(),
            unread: 0,
        })
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
        mut held: impl FnMut(&Op, bool) -> Result<bool, E>,
    ) -> Result<Option<Unconfirmed>, E> {
        // where frames it holds are gone the caller is not told: the
        // ones the handle wrote stay owed an answer
        self.read_to_end()?;
        self.check_synced()?;
        let from = self.judged.get();
        if from >= self.state.end {
            return Ok(None);
        }
        let Some(cut) = self.first_unheld(from, &mut held)? else {
            self.note_settled().map_err(OpLogError::from)?;
            self.judged.set(u64::MAX);
            return Ok(None);
        };
        let discarded = self.to_cut(from, cut)?;
        let kept = self.kept_past(&discarded, held)?;
        self.cut_at(cut.at, discarded, (true, kept))?;
        Ok(Some(cut))
    }

    /// Cuts off what follows the last frame held, which is no frame.
    /// Nobody looked at the ops before it for that.
    pub(super) fn cut_end(&mut self, discarded: Discarded) -> Result<(), OpLogError> {
        self.cut_at(self.state.end, discarded, (false, None))
    }

    /// What `tail` says after a cut at `at` of the ops another boot left:
    /// whether some are still to be looked at, and from where. `noted` is
    /// what it said of the log before the cut. A cut that followed a look
    /// at every op (`looked`) leaves none. Any other says no more than was
    /// known before it: the checkpoint it writes is of this boot, and tells
    /// nothing of the frames before it.
    fn left_after(&self, noted: Option<Tail>, at: u64, looked: bool) -> (bool, u64) {
        let judged = self.judged.get().min(at);
        let noted = noted.filter(|tail| tail.pending && tail.generation == self.state.generation);
        match (looked, noted) {
            (true, _) => (false, at),
            (false, Some(tail)) => (true, tail.judged.max(judged).min(at)),
            (false, None) => (judged < at, judged),
        }
    }

    /// Says where the log ends what `discarded` holds was cut, in a
    /// checkpoint of `generation` that is synced, and has the file end
    /// with it; `looked` for ops of the log that a look refused.
    fn lay_cut(
        &mut self,
        discarded: &Discarded,
        (generation, looked): (u32, bool),
    ) -> Result<(), OpLogError> {
        let cut = Cut {
            bytes: discarded.to - discarded.from,
            ops: discarded.ops.len() as u32,
            of_the_log: looked,
            after: self.state.cut,
            generation: self.state.generation,
        };
        let mut state = self.state.clone();
        state.generation = generation;
        let bytes = self.composer().checkpoint(&mut state, Some(cut))?;
        self.put(&bytes, state)?;
        cut_reached(2)?;
        crate::v2::fsync(&self.file)?;
        cut_reached(3)?;
        self.file.set_len(self.state.end)?;
        cut_reached(4)?;
        crate::v2::fsync(&self.file)?;
        cut_reached(5)?;
        Ok(())
    }

    /// The generation the log is of once it is cut: above the generation
    /// of every frame that reads in the file, also past where it is cut.
    /// A cut made again after the power went while its checkpoint was
    /// written takes the generation that one was of: nothing else was
    /// written under it. A cut fsyncs its checkpoint before it goes on,
    /// and a writer that finds the log to end with the checkpoint of a
    /// cut fsyncs it before it writes after it (`cut_fsynced`).
    fn generation_cut(&self) -> Result<u32, OpLogError> {
        let cut_before = OpLogError::Format("the oplog was cut as often as it can count");
        self.state.generation.checked_add(1).ok_or(cut_before)
    }

    /// Cuts the log at `at`, where a frame this handle holds starts or
    /// the last one ends, under the exclusive lock of the log. Nothing is
    /// cut that was recorded as synced. `looked` says that every op
    /// before `at` was vouched for, `kept` where the first frame past
    /// `at` starts that a writer who syncs left with what it names
    /// stored.
    ///
    /// How far the log is synced this boot knows from `tail`, where a
    /// writer left one for this log, from the frames and from what this
    /// handle synced itself. The last sync before the machine went down
    /// is in none of them until a frame was written after it, nor is the
    /// last sync of this boot once `tail` is gone. A frame such a sync
    /// may have covered is not cut: the cut is refused
    /// ([`OpLogError::Kept`]).
    ///
    /// The order holds through a power cut and through the death of this
    /// process alike. The header of the log first tells that it is cut
    /// at `at`: the frames past it stay readable until the file is made
    /// shorter, and are no log from the next step on. Then the log says
    /// what is cut, in a checkpoint under the next generation written
    /// over them and synced. Only then is the file made to end with that
    /// checkpoint. `tail` speaks of the log as it is once cut from the
    /// first step on: it says nothing of the log before, and whoever
    /// reads that one takes its ops for left to be looked at.
    fn cut_at(
        &mut self,
        at: u64,
        discarded: Discarded,
        (looked, kept): (bool, Option<u64>),
    ) -> Result<(), OpLogError> {
        self.may_write()?;
        // neither a frame that must be known is cut nor the log after one
        self.may_append()?;
        let held = Held::take(&self.dir)?;
        let noted = Tail::noted_held(&self.beside(), &held).says();
        let synced = self.may_cut(at, noted, kept)?;
        let (pending, judged) = self.left_after(noted, at, looked);
        let generation = self.generation_cut()?;
        if at != self.state.end {
            self.read_to(at)?;
            self.may_append()?;
        }
        Mark { generation, at }.write(&self.file)?;
        // the checkpoint of the cut is of this boot: whoever begins at it
        // finds noted what is left to look at before it
        let mut tail = Tail {
            pending,
            generation,
            closes: [0; 4],
            synced,
            judged: if pending { judged } else { at },
            hidden: 0,
            ends: 0,
        };
        tail.write(&self.beside(), &held)?;
        cut_reached(1)?;
        self.lay_cut(&discarded, (generation, looked))?;
        // the checkpoint is synced with the cut; of a frame this handle
        // wrote that was cut, its caller is told by the look
        self.unsynced = false;
        self.cut_on_disk = self.state.end;
        self.left_to = self.left_to.min(at);
        self.account.lock().unwrap().cut_at(at);
        if !pending {
            // with nothing left to look at, the checkpoint is covered too
            tail.judged = self.state.end;
        }
        tail.write(&self.beside(), &held)?;
        if looked {
            self.judged.set(u64::MAX);
        }
        self.hidden = None;
        self.discarded.push(discarded);
        Ok(())
    }
}
