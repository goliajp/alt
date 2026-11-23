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

    /// Notes in `tail` that the frames up to `judged` were looked at in
    /// this boot, and that what follows them in a file as long as it is
    /// now is no part of the log.
    fn note_judged(&self, judged: u64) -> std::io::Result<()> {
        let len = self.file.metadata()?.len();
        let generation = self.state.generation;
        Tail::update(&self.dir, self.boot, |tail| {
            tail.generation = generation;
            tail.judged = judged;
            tail.hidden = if len > judged { len } else { 0 };
        })
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
        match found {
            Some(found) => {
                let _ = self.note_judged(found.at);
            }
            None => {
                let _ = self.note_judged(self.state.end);
                self.judged.set(u64::MAX);
            }
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
            self.note_judged(self.state.end).map_err(OpLogError::from)?;
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
    /// the last one ends, under the exclusive lock of the log: the file
    /// is cut, then says so in a checkpoint under the next generation,
    /// and is synced. Nothing is cut that was recorded as synced.
    fn cut_at(&mut self, at: u64, discarded: Discarded) -> Result<(), OpLogError> {
        let held = Held::take(&self.dir)?;
        let synced = self.tail().map_or(0, |tail| tail.synced).max(self.synced);
        if at < synced {
            return Err(OpLogError::Lost { synced, holds: at });
        }
        self.file.set_len(at)?;
        if at != self.state.end {
            self.read_anew()?;
        }
        let cut = Cut {
            bytes: discarded.to - discarded.from,
            ops: discarded.ops.len() as u32,
        };
        let mut state = self.state.clone();
        state.generation += 1;
        let bytes = self.composer().checkpoint(&mut state, Some(cut))?;
        self.put(&bytes, state)?;
        self.file.sync_all()?;
        self.unsynced = false;
        let tail = Tail {
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
