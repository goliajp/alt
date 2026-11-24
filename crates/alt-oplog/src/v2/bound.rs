//! Up to where a handle takes the log for synced.
//!
//! It knows that from the frames it read, from its own syncs, and from
//! `tail`. What `tail` says, it says of the log of one generation: a
//! handle of another generation may hold frames that were cut since, in
//! the place of which the log that `tail` speaks of holds others. So a
//! handle takes the boundary noted there for its own only when the note
//! is of the generation it holds. A sync that finds the note to be of a
//! later one learns under `lock` whether its frames are in their place.

use crate::OpLogError;
use crate::v2::log::Log;
use crate::v2::tail::{Place, Tail};

impl Log {
    /// The log this handle is on, for whoever reads or writes the note
    /// beside it.
    pub(super) fn beside(&self) -> Place<'_> {
        Place {
            dir: &self.dir,
            boot: self.boot,
            id: self.layout.id,
            log: &self.file,
        }
    }

    /// Up to where `tail` says the log this handle holds is synced.
    pub(super) fn said_synced(&self, tail: Option<Tail>) -> u64 {
        tail.map_or(0, |tail| tail.synced_of(self.state.generation))
    }

    /// What `tail` says in the running boot; the boundary it says of the
    /// log this handle holds is taken in.
    pub(super) fn tail(&mut self) -> Option<Tail> {
        let tail = Tail::read(&self.beside());
        self.synced = self.synced.max(self.said_synced(tail));
        tail
    }

    /// Up to where the log this handle holds is noted as synced, by
    /// `tail` or by what the handle knows.
    pub(super) fn noted_bound(&mut self) -> u64 {
        self.tail();
        self.synced
    }

    pub fn bounded_to(&self) -> Option<u64> {
        let tail = Tail::read(&self.beside());
        Some(self.synced.max(self.said_synced(tail)))
    }

    pub fn unsynced_from(&self) -> Option<u64> {
        self.bounded_to()
    }

    /// The boundary, when frames follow it that were appended or read
    /// through in the running boot.
    pub fn left_unsynced_from(&self) -> Option<u64> {
        let synced = self.bounded_to()?;
        (synced < self.state.end && self.judged.get() >= self.state.end).then_some(synced)
    }

    pub fn confirmed(&self) -> bool {
        self.bounded_to()
            .is_some_and(|synced| synced >= self.state.end)
    }

    pub fn check_synced(&self) -> Result<(), OpLogError> {
        match self.bounded_to() {
            Some(synced) if self.state.end < synced => Err(OpLogError::Lost {
                synced,
                holds: self.state.end,
            }),
            _ => Ok(()),
        }
    }
}
