//! The claim of one caller to an answer for the frames a handle wrote
//! for it.
//!
//! A caller that shares a handle with others has the handle write for
//! it on a ticket: the frames are on the ticket from when they are
//! written, whatever others append before or after, and a sync asked
//! with the ticket answers for them and for no other frame. What a
//! handle writes otherwise is held by the handle itself, and answered
//! for by a sync asked without a ticket.
//!
//! A ticket that was told of a cut is told so whenever it asks again.
//! One that is dropped gives its frames back to the handle: whoever
//! asks that without a ticket is answered for them, and nothing stays
//! in the account for a caller that can no longer ask.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::v2::log::Log;
use crate::v2::owed::{Account, Holder, Shared};
use crate::{OpLogError, relaxed_durability};

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

/// The claim to an answer for frames a handle wrote: whether a sync is
/// recorded over them, or they were cut off the log. It is good for the
/// handle it was written on and for the sinks of that handle.
#[derive(Debug, Default)]
pub struct Ticket {
    numbers: Vec<u64>,
    account: Weak<Mutex<Account>>,
    /// its holder was told that frames on it were cut
    cut: AtomicBool,
    /// a sink answered that it has nothing to sync its frames up to:
    /// the handle syncs them itself when it is asked next
    owed: AtomicBool,
}

impl Ticket {
    pub(crate) fn of(account: &Shared, number: u64) -> Ticket {
        let mut ticket = Ticket::default();
        (ticket.numbers, ticket.account) = (vec![number], Arc::downgrade(account));
        ticket
    }

    /// Whether no frame was written on it: a sync asked with it answers
    /// for nothing, and succeeds.
    pub fn is_empty(&self) -> bool {
        self.numbers.is_empty()
    }

    /// Takes the frames of `other` onto this ticket: they are answered
    /// for together from now on. Where either was told of a cut, the
    /// one ticket is told so whenever it asks, also for frames that
    /// are in the log: callers to be answered apart keep their tickets
    /// apart.
    pub fn join(&mut self, mut other: Ticket) {
        if self.numbers.is_empty() {
            self.account = other.account.clone();
        }
        self.numbers.append(&mut other.numbers);
        *self.cut.get_mut() |= *other.cut.get_mut();
        *self.owed.get_mut() |= *other.owed.get_mut();
    }

    pub(crate) fn holds(&self, holder: Holder) -> bool {
        matches!(holder, Holder::Ticket(number) if self.numbers.contains(&number))
    }

    /// Refuses a ticket of another log than the one of `account`.
    pub(crate) fn asked_of(&self, account: &Shared) -> Result<(), OpLogError> {
        let its_own = std::ptr::eq(self.account.as_ptr(), Arc::as_ptr(account));
        match self.numbers.is_empty() || its_own {
            true => Ok(()),
            false => Err(OpLogError::Format("a ticket of another oplog")),
        }
    }

    /// Whether its holder was told that frames on it were cut.
    pub(crate) fn told_cut(&self) -> bool {
        self.cut.load(Ordering::Relaxed)
    }

    /// Whether a sink answered last that it has nothing to sync the
    /// frames up to.
    pub(crate) fn owed(&self) -> bool {
        self.owed.load(Ordering::Relaxed)
    }

    /// Its holder is told that frames on it were cut.
    pub(crate) fn tell_cut(&self) {
        self.cut.store(true, Ordering::Relaxed);
    }

    /// A sink answered that it has nothing to sync its frames up to,
    /// or, with `false`, they were answered for otherwise since.
    pub(crate) fn owes(&self, owed: bool) {
        self.owed.store(owed, Ordering::Relaxed);
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let Some(account) = self.account.upgrade() else {
            return;
        };
        // one that was told of a cut, or that a sync is recorded over,
        // holds nothing
        if let Ok(mut account) = account.lock() {
            account.given_back(&self.numbers);
        }
    }
}

/// While it lives, what the handle it was begun on writes is on one
/// ticket, which it gives at last ([`Self::ticket`]). Dropped without
/// that, it leaves what was written to the handle. The last one begun
/// is the first to end: only what runs a write for a caller from its
/// beginning to its end holds one.
#[derive(Default)]
pub(crate) struct Writing {
    ticket: Option<Ticket>,
    account: Option<Shared>,
}

impl Writing {
    /// Has the frames written through the handle of `account` from now
    /// on be on a new ticket.
    pub(crate) fn begin(account: &Shared) -> Writing {
        let number = account.lock().unwrap().writes_on_a_ticket();
        Writing {
            ticket: Some(Ticket::of(account, number)),
            account: Some(account.clone()),
        }
    }

    fn end(&mut self) -> Option<Shared> {
        let account = self.account.take()?;
        let numbers = self.ticket.iter().flat_map(|ticket| &ticket.numbers);
        if let Ok(mut held) = account.lock() {
            numbers.for_each(|number| held.writes_no_longer_on(*number));
        }
        Some(account)
    }

    /// The ticket of what was written, with which the handle writes on
    /// it no longer: one that holds nothing where nothing was written.
    pub fn ticket(mut self) -> Ticket {
        let account = self.end();
        let ticket = self.ticket.take().unwrap_or_default();
        let written = account.is_some_and(|account| {
            let account = account.lock().unwrap();
            account.holds(&|holder| ticket.holds(holder))
        });
        match written {
            true => ticket,
            false => Ticket::default(),
        }
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        self.end();
    }
}

impl Log {
    /// Has the frames this handle writes be on a new ticket for as
    /// long as what is given lives.
    pub fn writing_on_a_ticket(&mut self) -> Writing {
        Writing::begin(&self.account)
    }

    /// [`Self::sync`] for the holder of `ticket`. A handle that leaves
    /// its syncs to a sink syncs by itself for a ticket the sink
    /// answered it has nothing to sync up to: what it answers then is
    /// what became of the frames.
    pub fn sync_for(&mut self, ticket: &Ticket) -> Result<(), OpLogError> {
        ticket.asked_of(&self.account)?;
        if ticket.told_cut() {
            return Err(OpLogError::CutBack);
        }
        let of = |holder| ticket.holds(holder);
        if relaxed_durability() || !self.writable {
            return self.promises_nothing(&of);
        }
        let leaves = self.defer && !ticket.owed();
        let synced = match leaves {
            false => self.sync_as(&of, true),
            true => {
                self.write_count += 1;
                self.answered(&of, true).and_then(|()| self.leave(&of))
            }
        };
        match &synced {
            Err(OpLogError::CutBack) => ticket.tell_cut(),
            Ok(()) if !leaves => ticket.owes(false),
            _ => {}
        }
        synced
    }
}

#[cfg(all(test, feature = "hooks"))]
mod tests {
    use crate::v2::log::Log;
    use crate::v2::owed::Holder;
    use crate::{Meta, Op, OpLogError};

    fn append(log: &mut Log) {
        let payload = |_: &[Op]| Ok::<_, OpLogError>(b"x".to_vec());
        log.append_checked("a", 1, &Meta::default(), payload)
            .unwrap();
    }

    #[test]
    fn a_frame_is_on_the_ticket_begun_last_that_has_not_ended_in_whatever_order_they_end() {
        let dir = alt_os::temp::tempdir().unwrap();
        crate::v2::in_boot(Some([1; 16]));
        crate::v2::create_with(dir.path(), 12).unwrap();
        let mut log = Log::open_at(dir.path(), None).unwrap();
        let first = log.writing_on_a_ticket();
        let second = log.writing_on_a_ticket();
        // the one begun first ends first
        let first = first.ticket();
        append(&mut log);
        let second = second.ticket();
        append(&mut log);
        assert!(first.is_empty() && !second.is_empty());
        let account = log.account.lock().unwrap();
        assert_eq!(account.len(), (2, 0));
        assert!(account.owes(&|holder| second.holds(holder)));
        assert!(account.owes(&|holder| holder == Holder::Own));
    }
}
