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
//! The rule is one: the log is not cut where a frame past the cut may
//! have been covered by a sync that succeeded. What the ops of the log
//! name tells whether one can have. A sync sees to every ref
//! transaction it covers, and to the op of each workspace that last
//! changed its index before where the sync ends: what such an op names
//! is stored before the log is recorded as synced past it. So a frame
//! past a ref transaction that names what is not stored was covered by
//! no sync. Nor was one past a change of an index that names what is
//! not stored, where no op after that one in all the log replaces it or
//! takes its workspace away. Where one does, a sync that ends past the
//! op which replaces it saw to that op instead, and covered the frames
//! in between as well. And where the log was cut past the change, an
//! op that replaced it may have been among what was cut: nothing tells
//! then that every sync saw to it. Past any other op that names what
//! is not stored a frame may have been covered too. Such a frame is one to keep when
//! a writer who syncs left it and what it names is stored; a receipt,
//! which names the ops of its request, when none of them is cut.

use crate::v2::frame::{KIND_RECEIPT, UNSYNCED, View};
use crate::v2::log::Log;
use crate::v2::receipt::Receipt;
use crate::v2::state::State;
use crate::v2::stream::walk_file;
use crate::v2::tail::Tail;
use crate::{Discarded, Op, OpLogError};

impl Log {
    /// Where the receipts start, past the first op of `cut`, that a
    /// writer who syncs left of a request none of whose ops is cut; in
    /// the order of the log.
    fn receipts_past(&self, cut: &Discarded) -> Result<Vec<u64>, OpLogError> {
        let of_cut = |receipt: Receipt| {
            let of_it = |op: &Op| op.frame.and_then(|frame| frame.request) == Some(receipt.request);
            cut.ops.iter().any(of_it)
        };
        let source = self.source();
        let mut read = |at: u64, len: usize| source.at(at, len);
        let (mut state, mut found) = (State::empty(), Vec::new());
        let mut each = |frame: u64, _: u64, view: &View| {
            let kept = frame > cut.from
                && view.head.kind == KIND_RECEIPT
                && view.head.flags & UNSYNCED == 0
                && Receipt::decode(view.body).is_some_and(|receipt| !of_cut(receipt));
            if kept {
                found.push(frame);
            }
            Ok(())
        };
        let end = self.state.end;
        walk_file(&mut read, &self.layout, &mut state, end, false, &mut each)?;
        Ok(found)
    }

    /// For each op of `cut`, whether every sync that covers a frame past
    /// it sees to it: a ref transaction, or a change of the index of a
    /// workspace that no op after it replaces, nor may have replaced
    /// before the log was last cut.
    fn seen_to_by_all(&self, cut: &Discarded) -> Vec<bool> {
        let mut later: Vec<[u8; 8]> = Vec::new();
        let mut seen: Vec<bool> = Vec::new();
        for frame in cut.ops.iter().rev().map(|op| op.frame) {
            let Some(frame) = frame else {
                seen.push(false);
                continue;
            };
            let of_workspace = frame.workspace != [0; 8];
            let replaced = later.contains(&frame.workspace);
            let changes = of_workspace && frame.staged && !frame.retired;
            let last = changes && !replaced && frame.at >= self.state.cut;
            seen.push(frame.linked || last);
            if of_workspace && (frame.staged || frame.retired) && !replaced {
                later.push(frame.workspace);
            }
        }
        seen.reverse();
        seen
    }

    /// Where the first frame starts, past the first op of `cut`, that a
    /// sync may have covered: a writer who syncs left it, `held` finds
    /// what it names stored, and no op before it that every sync past
    /// it sees to names what is not.
    pub(super) fn kept_past<E: From<OpLogError>>(
        &self,
        cut: &Discarded,
        mut held: impl FnMut(&Op, bool) -> Result<bool, E>,
    ) -> Result<Option<u64>, E> {
        let seen_to = self.seen_to_by_all(cut);
        // what the op refused names is not stored
        if seen_to.first() != Some(&false) {
            return Ok(None);
        }
        let receipts = self.receipts_past(cut)?;
        let mut receipts = receipts.into_iter().peekable();
        let ops = cut.ops.iter().zip(&seen_to).skip(1);
        for (op, seen_to) in ops {
            let Some(frame) = op.frame else {
                continue;
            };
            if let Some(receipt) = receipts.next_if(|receipt| *receipt < frame.at) {
                return Ok(Some(receipt));
            }
            // of the ops past the one refused, those are asked about
            // that the answer turns on
            let unsynced = frame.flags & UNSYNCED != 0;
            if !seen_to && unsynced {
                continue;
            }
            match (held(op, unsynced)?, unsynced) {
                (true, false) => return Ok(Some(frame.at)),
                (false, _) if *seen_to => return Ok(None),
                _ => {}
            }
        }
        Ok(receipts.next())
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
}
