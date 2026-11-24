//! Frames past an op a writer is about to cut the log at that a sync may
//! have covered.
//!
//! A writer that syncs answers its command once the log is recorded as
//! synced past its frames, and that is recorded beside the log alone
//! until the next frame is written. What is noted beside the log is
//! good for the boot it was noted in, and may be gone before that: of a
//! frame past where the frames say the log is synced, nothing tells for
//! sure that no sync covered it and no command was answered for it.
//!
//! What the op refused is tells more. A sync sees to every ref
//! transaction it covers, and to the op of each workspace that last
//! changed its index: what such an op names is stored before a frame
//! past it is recorded as synced. So where the op refused is one of
//! those, no sync covered a frame past it, and the cut takes them. An
//! op a sync does not see to names what may be gone though a sync
//! covered it: a change of an index that a later op of its workspace
//! replaced, or an op that is neither. A frame past such an op that a
//! writer who syncs left, with what it names stored, is one to keep, and
//! the log is not cut before it. A receipt names the ops of its
//! request: it is one to keep when none of them is cut.

use crate::v2::frame::{KIND_RECEIPT, UNSYNCED, View};
use crate::v2::log::Log;
use crate::v2::receipt::Receipt;
use crate::v2::state::State;
use crate::v2::stream::walk_file;
use crate::v2::tail::Tail;
use crate::{Discarded, Op, OpLogError};

impl Log {
    /// Where the first receipt past the first op of `cut` starts that a
    /// writer who syncs left of a request none of whose ops is cut.
    fn receipt_past(&self, cut: &Discarded) -> Result<Option<u64>, OpLogError> {
        let of_cut = |receipt: Receipt| {
            let of_it = |op: &Op| op.frame.and_then(|frame| frame.request) == Some(receipt.request);
            cut.ops.iter().any(of_it)
        };
        let source = self.source();
        let mut read = |at: u64, len: usize| source.at(at, len);
        let (mut state, mut found) = (State::empty(), None);
        let mut each = |frame: u64, _: u64, view: &View| {
            let kept = frame > cut.from
                && found.is_none()
                && view.head.kind == KIND_RECEIPT
                && view.head.flags & UNSYNCED == 0
                && Receipt::decode(view.body).is_some_and(|receipt| !of_cut(receipt));
            if kept {
                found = Some(frame);
            }
            Ok(())
        };
        let end = self.state.end;
        walk_file(&mut read, &self.layout, &mut state, end, false, &mut each)?;
        Ok(found)
    }

    /// Where the first frame starts, past the first op of `cut`, that a
    /// writer who syncs left, whose content `held` finds stored, and that
    /// a sync may have covered without seeing to that first op.
    pub(super) fn kept_past<E: From<OpLogError>>(
        &self,
        cut: &Discarded,
        mut held: impl FnMut(&Op, bool) -> Result<bool, E>,
    ) -> Result<Option<u64>, E> {
        let Some(refused) = cut.ops.first().and_then(|op| op.frame) else {
            return Ok(None);
        };
        if refused.linked {
            return Ok(None);
        }
        // a change of the index of a workspace is seen to until a later
        // op of the workspace replaces it or the workspace is gone
        let of_workspace = refused.workspace != [0; 8];
        let mut unseen = !(refused.staged && of_workspace);
        let mut receipt = self.receipt_past(cut)?;
        for op in cut.ops.iter().skip(1) {
            let Some(frame) = op.frame else {
                continue;
            };
            if receipt.is_some_and(|receipt| receipt < frame.at) {
                if unseen {
                    return Ok(receipt);
                }
                receipt = None;
            }
            let replaces = frame.workspace == refused.workspace && (frame.staged || frame.retired);
            unseen |= of_workspace && replaces;
            if unseen && frame.flags & UNSYNCED == 0 && held(op, false)? {
                return Ok(Some(frame.at));
            }
        }
        Ok(receipt.filter(|_| unseen))
    }

    /// Up to where the log is known synced to a writer about to cut it
    /// at `at`, which `tail` says `noted` to; the cut is refused within
    /// that, and before a frame to keep at `kept`. This writer holds the
    /// log as it is: a note of a generation after is of a cut that never
    /// came to it, and says how far the log was synced when that began;
    /// one of a generation before was left by a handle that knew of no
    /// sync since the log was cut.
    pub(super) fn may_cut(
        &mut self,
        at: u64,
        noted: Option<Tail>,
        kept: Option<u64>,
    ) -> Result<u64, OpLogError> {
        let said = noted.filter(|tail| tail.generation >= self.state.generation);
        let known = self.synced.max(self.state.synced).max(self.synced_here());
        let synced = said.map_or(0, |tail| tail.synced).max(known);
        self.synced = synced;
        if at < synced {
            return Err(OpLogError::Lost { synced, holds: at });
        }
        match kept {
            Some(kept) => Err(OpLogError::Kept { unheld: at, kept }),
            None => Ok(synced),
        }
    }

    /// The cut at `at` is made: its checkpoint is synced with it and
    /// awaits no answer. The frame this handle wrote before the cut, and
    /// whether it awaited an answer from a sync, is `wrote`: one before
    /// `at` awaits its answer as before; of one that was cut the caller
    /// is told by the look.
    pub(super) fn cut_laid(&mut self, at: u64, wrote: ((u64, [u8; 4]), bool)) {
        self.unsynced = false;
        (self.wrote, self.owes) = match wrote.0.0 > at {
            true => (self.wrote, false),
            false => wrote,
        };
    }
}
