//! The ops a sync has to see to before it records the log as synced over
//! them.
//!
//! A handle holds the ops from the checkpoint it began at on, and a sync
//! may reach further back: other writers appended frames it never read.
//! Of those it takes what names something that has to be on disk first.
//! That is every ref transaction, found from the last one by the frame
//! each names as the one before it, and of each workspace the op that
//! last changed its index, which the table of workspaces tells: an
//! earlier change of the same index is replaced by it. What is read for
//! this is a frame for each such op and not the frames in between.

use crate::v2::log::Log;
use crate::{Op, OpLogError};

/// a frame a sync has to see to does not read
const UNREAD: OpLogError = OpLogError::Format("oplog record corrupt");

impl Log {
    /// The ops whose frames start at or after `from` that a sync has to
    /// see to, in the order of the log: all that this handle holds, and
    /// before those the ref transactions and the last op of each
    /// workspace that changed its index.
    pub(super) fn named_from(&self, from: u64) -> Result<Vec<Op>, OpLogError> {
        let at_of = |op: &Op| op.frame.map_or(0, |frame| frame.at);
        let held = self.ops.iter().filter(|op| at_of(op) >= from);
        let held: Vec<Op> = held.cloned().collect();
        if from >= self.began {
            return Ok(held);
        }
        let holds = |at: u64| held.iter().any(|op| at_of(op) == at);
        let mut before = Vec::new();
        let mut at = self.began_linked;
        while at >= from && at != 0 {
            let Some((op, Some(earlier))) = self.op_at(at)? else {
                return Err(UNREAD);
            };
            if earlier >= at {
                return Err(UNREAD);
            }
            if !holds(at) {
                before.push((at, op));
            }
            at = earlier;
        }
        for row in &self.state.workspaces {
            let at = row.index_at;
            let known = holds(at) || before.iter().any(|(known, _)| *known == at);
            if (from..self.began).contains(&at) && !known {
                let Some((op, _)) = self.op_at(at)? else {
                    return Err(UNREAD);
                };
                before.push((at, op));
            }
        }
        let mut ops: Vec<Op> = before.into_iter().map(|(_, op)| op).collect();
        ops.extend(held);
        ops.sort_by_key(at_of);
        Ok(ops)
    }
}
