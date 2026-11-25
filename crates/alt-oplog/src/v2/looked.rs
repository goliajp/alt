//! Ops past where the log is synced that nobody looked at, and that a
//! sync does not see to.
//!
//! A sync sees to the ops it covers that name what has to be on disk
//! first (`named`): what such an op names is stored before the log is
//! recorded as synced over it, whoever left the op. Of any other op it
//! knows nothing. One of those that another boot left names what may
//! never have reached the disk, and is for a writer to look at
//! (`settle`): until one has, no boundary is set over it. A handle that
//! was given nobody to sync what ops name sees to none.
//!
//! What was appended since the handle read the log as an open does, or
//! after a cut made since, was appended in the running boot, and is none
//! to look at. Nor is anything once somebody of the running boot has
//! vouched for all the handle read then.

use crate::v2::log::Log;
use crate::{Framed, OpLogError};

impl Log {
    /// Where the first op starts, among those that end by byte `to`,
    /// that nobody has looked at and that a sync up to `to` does not
    /// see to.
    pub(super) fn unlooked(&self, to: u64) -> Result<Option<u64>, OpLogError> {
        let from = self.judged.get().max(self.synced);
        if from >= to {
            return Ok(None);
        }
        let ops = self.ops_past(from)?;
        let frames = ops.iter().filter_map(|op| op.frame);
        let within = |frame: &Framed| frame.at >= from && frame.end <= to;
        let frames: Vec<Framed> = frames.filter(within).collect();
        let left = |frame: &&Framed| frame.at < self.left_to;
        if self.named.is_none() {
            return Ok(frames.iter().find(left).map(|frame| frame.at));
        }
        // from the last on: a change of an index is seen to while no
        // later op of its workspace replaces it or takes the workspace
        // away
        let (mut later, mut first) = (Vec::new(), None);
        for frame in frames.iter().rev() {
            let of_workspace = frame.workspace != [0; 8];
            let replaced = later.contains(&frame.workspace);
            let changes = of_workspace && frame.staged && !frame.retired;
            if !(frame.linked || (changes && !replaced)) && left(&frame) {
                first = Some(frame.at);
            }
            if of_workspace && (frame.staged || frame.retired) && !replaced {
                later.push(frame.workspace);
            }
        }
        Ok(first)
    }

    /// Refuses to set a boundary at byte `to` over an op nobody looked
    /// at that a sync does not see to; the handle holds no lock on the
    /// log. One that is gone from the log is none to look at: the
    /// handle is told that ops it holds were cut.
    pub(super) fn looked_to(&mut self, to: u64) -> Result<(), OpLogError> {
        if self.judged.get() >= to || self.vouched_since()? {
            return Ok(());
        }
        let Some(at) = self.unlooked(to)? else {
            return Ok(());
        };
        match self.cut_from()? {
            Some(_) => Err(OpLogError::CutBack),
            None => Err(OpLogError::Unjudged { at }),
        }
    }
}
