//! The account of the frames a handle wrote that no sync is recorded
//! over, and who is answered for each.
//!
//! A frame is owed an answer from when it is written: a sync is recorded
//! over it, and it is owed none any longer, or it is found cut off the
//! log, and whoever holds it is told so by the next sync that one asks
//! for, once. Nobody else is: a caller whose frames are in the log is not
//! told of a cut for the frames of another.
//!
//! Who holds a frame is settled when it is written: the handle itself,
//! or the ticket the handle was writing on (`ticket`). A sync asked
//! without a ticket answers for all the frames on none, as for the
//! frames of one caller; a caller that shares the handle with others
//! has its frames written on a ticket and asks with that. What is on a
//! ticket nobody asks with stays in the account, told to nobody, and
//! makes no sync of another fail, until the ticket is dropped: then the
//! handle holds it.
//!
//! An entry leaves the account when a sync is recorded over its frame,
//! when its holder is told that it was cut, and when its holder is
//! answered with nothing promised, by a sync that was told to leave
//! what is written unsynced.
//!
//! The handle shares the account with its sink, which finds frames cut
//! as the handle does and answers by the same tickets.

use std::sync::{Arc, Mutex};

use crate::OpLogError;
use crate::v2::sink::Reached;

/// Who is answered for a frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Holder {
    /// whoever asks the handle with no ticket
    #[default]
    Own,
    /// whoever asks the sink of the handle with no ticket: the handle
    /// left the frame to the sink
    Left,
    Ticket(u64),
}

/// What became of the frames on a ticket that were not cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// a sync is recorded over every one of them
    Synced,
    /// They are in the log, no sync is recorded over them yet, and the
    /// sink has nothing to sync them up to. The handle is asked with
    /// the ticket ([`crate::OpLog::sync_for`]): it syncs them itself
    /// then, and what it answers is final.
    Owed,
}

/// A frame the handle wrote that no sync is recorded over.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Owed {
    /// where it ends, and the four bytes of the file before that
    pub end: u64,
    pub closes: [u8; 4],
    /// the generation the log was of when it was written
    pub generation: u32,
    holder: Holder,
}

#[derive(Debug, Default)]
pub(crate) struct Account {
    /// the frames owed an answer, in the order they were written
    owed: Vec<Owed>,
    /// who holds frames that were found cut and was not told yet
    cut: Vec<Holder>,
    /// how many tickets frames were written on
    tickets: u64,
    /// who holds what the handle writes now
    writing: Holder,
    /// what the handle last left for its sink to sync
    pub left: Reached,
    /// the furthest boundary the handle or its sink saw recorded
    pub synced: u64,
    /// the sink found frames the handle holds out of their place: the
    /// handle reads the log again before it syncs
    pub stale: bool,
    /// of some frames owed, the last look could not tell whether they
    /// are cut: a cut of them was begun and not made
    pub unsure: bool,
}

pub(crate) type Shared = Arc<Mutex<Account>>;

impl Account {
    /// The handle wrote a frame.
    pub fn wrote(&mut self, end: u64, closes: [u8; 4], generation: u32) {
        self.owed.push(Owed {
            end,
            closes,
            generation,
            holder: self.writing,
        });
    }

    /// Has what the handle writes from now on be on a new ticket; gives
    /// its number, and who held what the handle wrote before.
    pub fn writes_on_a_ticket(&mut self) -> (u64, Holder) {
        self.tickets += 1;
        let before = std::mem::replace(&mut self.writing, Holder::Ticket(self.tickets));
        (self.tickets, before)
    }

    pub fn writes_for(&mut self, holder: Holder) {
        self.writing = holder;
    }

    /// The tickets of `numbers` are dropped: what they held, the handle
    /// holds.
    pub fn given_back(&mut self, numbers: &[u64]) {
        for number in numbers {
            let ticket = Holder::Ticket(*number);
            for owed in &mut self.owed {
                if owed.holder == ticket {
                    owed.holder = Holder::Own;
                }
            }
            if self.cut.contains(&ticket) {
                self.cut.retain(|holder| *holder != ticket);
                self.found_cut(Holder::Own);
            }
        }
    }

    /// Whether `of` holds a frame that is owed an answer or was found
    /// cut.
    pub fn holds(&self, of: &dyn Fn(Holder) -> bool) -> bool {
        self.owes(of) || self.found_cut_of(of)
    }

    /// How many frames are owed an answer, and how many holders are to
    /// be told of a cut.
    #[cfg(test)]
    pub fn len(&self) -> (usize, usize) {
        (self.owed.len(), self.cut.len())
    }

    /// Whether a frame `of` holds is owed an answer.
    pub fn owes(&self, of: &dyn Fn(Holder) -> bool) -> bool {
        self.owed.iter().any(|owed| of(owed.holder))
    }

    /// Whether a frame `of` holds that ends past byte `end` is owed an
    /// answer.
    pub fn owes_past(&self, of: &dyn Fn(Holder) -> bool, end: u64) -> bool {
        let mut owed = self.owed.iter();
        owed.any(|owed| of(owed.holder) && owed.end > end)
    }

    /// The last frame owed that ends by byte `end`.
    pub fn last_by(&self, end: u64) -> Option<(u64, [u8; 4])> {
        let within = self.owed.iter().rev().find(|owed| owed.end <= end);
        within.map(|owed| (owed.end, owed.closes))
    }

    /// Whether `of` holds a frame that was found cut, and has not been
    /// told.
    pub fn found_cut_of(&self, of: &dyn Fn(Holder) -> bool) -> bool {
        self.cut.iter().any(|holder| of(*holder))
    }

    /// `of` is answered for nothing any longer.
    pub fn spend(&mut self, of: &dyn Fn(Holder) -> bool) {
        self.cut.retain(|holder| !of(*holder));
        self.owed.retain(|owed| !of(owed.holder));
    }

    /// Answers whoever `of` holds frames for. Where some of them were
    /// cut, that one is told so, once ([`OpLogError::CutBack`]); with
    /// `spent`, nothing of it is owed an answer after that.
    pub fn answer(
        &mut self,
        of: &dyn Fn(Holder) -> bool,
        spent: bool,
    ) -> Result<Answer, OpLogError> {
        if self.found_cut_of(of) {
            match spent {
                true => self.spend(of),
                false => self.cut.retain(|holder| !of(*holder)),
            }
            return Err(OpLogError::CutBack);
        }
        match self.owes(of) {
            true => Ok(Answer::Owed),
            false => Ok(Answer::Synced),
        }
    }

    /// Has `to` hold what `from` holds; whether that was anything.
    pub fn hand(&mut self, from: Holder, to: Holder) -> bool {
        let mut any = false;
        let owed = self.owed.iter_mut().map(|owed| &mut owed.holder);
        for holder in owed.chain(&mut self.cut) {
            if *holder == from {
                (*holder, any) = (to, true);
            }
        }
        any
    }

    /// A sync is recorded up to byte `target`, over frames that are in
    /// their place.
    pub fn covered(&mut self, target: u64) {
        self.synced = self.synced.max(target);
        self.owed.retain(|owed| owed.end > target);
    }

    /// The log is fsynced up to byte `target`, over frames that are in
    /// their place, with no sync recorded: nobody saw to what the ops
    /// up to there name.
    pub fn fsynced(&mut self, target: u64) {
        self.owed.retain(|owed| owed.end > target);
    }

    fn found_cut(&mut self, holder: Holder) {
        if !self.cut.contains(&holder) {
            self.cut.push(holder);
        }
    }

    /// The handle cut the log at byte `at` itself: of a frame it holds
    /// itself its caller is told by that cut.
    pub fn cut_at(&mut self, at: u64) {
        let owed = self.owed.iter().copied();
        let (kept, gone): (Vec<Owed>, Vec<Owed>) = owed.partition(|owed| owed.end <= at);
        self.owed = kept;
        for owed in gone.iter().filter(|owed| owed.holder != Holder::Own) {
            self.found_cut(owed.holder);
        }
    }

    /// Whoever asks the handle with no ticket was told that frames were
    /// cut.
    pub fn told(&mut self) {
        self.cut.retain(|holder| *holder != Holder::Own);
    }

    /// Looks at every frame owed with `placed`, which says whether the
    /// frame is where it was written, or `None` where it cannot tell.
    /// One that is not was cut.
    pub fn look<E>(
        &mut self,
        mut placed: impl FnMut(&Owed) -> Result<Option<bool>, E>,
    ) -> Result<(), E> {
        let mut looked = Ok(());
        self.unsure = false;
        for owed in std::mem::take(&mut self.owed) {
            // what is not looked at stays owed
            let placed = match looked {
                Ok(()) => placed(&owed),
                Err(_) => Ok(None),
            };
            match placed {
                Ok(Some(false)) => self.found_cut(owed.holder),
                Ok(sure) => {
                    self.unsure |= sure.is_none();
                    self.owed.push(owed);
                }
                Err(error) => {
                    self.owed.push(owed);
                    looked = Err(error);
                }
            }
        }
        looked
    }
}
