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

use crate::OpLogError;
use crate::v2::owed::{Account, Holder, Shared};

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
    /// for together from now on.
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
/// that, it leaves what was written to the handle.
#[derive(Default)]
pub struct Writing {
    ticket: Option<Ticket>,
    account: Option<Shared>,
    before: Holder,
}

impl Writing {
    /// Has the frames written through the handle of `account` from now
    /// on be on a new ticket.
    pub(crate) fn begin(account: &Shared) -> Writing {
        let (number, before) = account.lock().unwrap().writes_on_a_ticket();
        Writing {
            ticket: Some(Ticket::of(account, number)),
            account: Some(account.clone()),
            before,
        }
    }

    fn end(&mut self) -> Option<Shared> {
        let account = self.account.take()?;
        if let Ok(mut held) = account.lock() {
            held.writes_for(self.before);
        }
        Some(account)
    }

    /// The ticket of what was written, with which the handle writes on
    /// it no longer: one that holds nothing where nothing was written.
    pub fn ticket(mut self) -> Ticket {
        let ticket = self.ticket.take().unwrap_or_default();
        let written = self.end().is_some_and(|account| {
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
