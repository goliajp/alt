//! The ops a sync has to see to before it records the log as synced over
//! them.
//!
//! They are the ops that name something which has to be on disk first:
//! every ref transaction, and of each workspace the op that last changed
//! its index, which the table of workspaces tells. An earlier change of
//! the same index is replaced by that one, whose op names the index
//! before it and the one after. Which ops these are follows from the log
//! alone, not from where a handle began to read it.
//!
//! A handle holds the ops from the checkpoint it began at on, and a sync
//! may reach further back: other writers appended frames it never read.
//! The ref transactions there are found from the last one by the frame
//! each names as the one before it, the others at the place the table
//! gives. What is read for this is a frame for each such op and not the
//! frames in between.

use crate::v2::log::Log;
use crate::{Op, OpLogError};

/// a frame a sync has to see to does not read
const UNREAD: OpLogError = OpLogError::Format("oplog record corrupt");

fn at_of(op: &Op) -> u64 {
    op.frame.map_or(0, |frame| frame.at)
}

impl Log {
    /// The op this handle holds whose frame starts at `at`.
    fn held_at(&self, at: u64) -> Option<&Op> {
        let found = self.ops.binary_search_by_key(&at, at_of);
        found.ok().map(|index| &self.ops[index])
    }

    /// The ops whose frames start at or after `from` that a sync has to
    /// see to, in the order of the log: the ref transactions and the op
    /// of each workspace that last changed its index.
    pub(super) fn named_from(&self, from: u64) -> Result<Vec<Op>, OpLogError> {
        self.named_before(from, u64::MAX)
    }

    /// [`Self::named_from`], with the frames that start at or after
    /// `cut` passed over where they do not read, and the ones only they
    /// lead to with them.
    fn named_before(&self, from: u64, cut: u64) -> Result<Vec<Op>, OpLogError> {
        let linked = |op: &&Op| op.frame.is_some_and(|frame| frame.linked);
        let held = self.ops.iter().filter(|op| at_of(op) >= from);
        let mut ops: Vec<Op> = held.filter(linked).cloned().collect();
        // the ref transactions before the ops held
        let mut at = self.began_linked;
        while at >= from && at != 0 && from < self.began {
            let (op, earlier) = match self.op_at(at) {
                Ok(Some((op, Some(earlier)))) if earlier < at => (op, earlier),
                _ if at >= cut => break,
                read => return Err(read.err().unwrap_or(UNREAD)),
            };
            if self.held_at(at).is_none() {
                ops.push(op);
            }
            at = earlier;
        }
        for row in &self.state.workspaces {
            let at = row.index_at;
            if at == 0 || at < from || ops.iter().any(|op| at_of(op) == at) {
                continue;
            }
            match (self.held_at(at), at >= cut) {
                (Some(op), _) => ops.push(op.clone()),
                (None, passed) => match self.op_at(at) {
                    Ok(Some((op, _))) => ops.push(op),
                    _ if passed => {}
                    read => return Err(read.err().unwrap_or(UNREAD)),
                },
            }
        }
        ops.sort_by_key(at_of);
        Ok(ops)
    }

    /// [`Self::named_from`] for a handle that holds no lock on the log.
    /// An op that does not read where the frames held say it is, because
    /// the log was cut since the handle read it or is being cut, is no
    /// damage: the handle is told that its ops were cut. One that lies
    /// before where the log is cut is damage as ever.
    pub(super) fn named_unlocked(&self, from: u64) -> Result<Vec<Op>, OpLogError> {
        let named = self.named_from(from);
        if named.is_err()
            && let Some(cut) = self.cut_from()?
            && self.named_before(from, cut).is_ok()
        {
            return Err(OpLogError::CutBack);
        }
        named
    }
}
