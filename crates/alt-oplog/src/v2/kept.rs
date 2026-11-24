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
//! name tells whether one can have. A sync ends where a frame ends. It
//! sees to every ref transaction between the boundary it found and
//! there, and to the op of each workspace that last changed its index
//! as the log stood up to there, where that op lies past the boundary:
//! what such an op names is stored before the log is recorded as synced
//! past it, and stays. So a sync can have ended at a frame only where
//! all those ops name what is stored, counted from the op refused or
//! from where an earlier sync that can have been made ended. A frame is
//! one to keep when a sync can have ended at it or past it, a writer
//! who syncs left it, and what it names is stored; a receipt, which
//! names the ops of its request, when none of them is cut.
//!
//! Where the log was cut past the op refused and the cut took ops, a
//! sync may have ended among what that cut took, and what it saw to
//! there is not known: an op that replaced a change of an index may
//! have been among it. Such a sync covered every frame before that
//! cut, and can have been made where the ref transactions before the
//! cut name what is stored. So with each cut past the op refused.

use crate::v2::frame::{KIND_RECEIPT, UNSYNCED, View};
use crate::v2::log::Log;
use crate::v2::receipt::Receipt;
use crate::v2::state::State;
use crate::v2::stream::walk_file;
use crate::v2::tail::Tail;
use crate::{Discarded, Framed, Op, OpLogError};

/// The ops from one a look refused on, and what is known of them so far.
struct Past<'a, H> {
    ops: &'a [Op],
    frames: Vec<Framed>,
    /// whether what each op names is stored, where `held` was asked
    stored: Vec<Option<bool>>,
    held: H,
    /// of each workspace, the op that last touched its index among the
    /// ops taken in
    last: Vec<([u8; 8], usize)>,
    /// how far the ops were looked at for one to keep, and where the
    /// first starts
    looked: usize,
    keeps: Option<u64>,
}

impl<'a, H, E> Past<'a, H>
where
    H: FnMut(&Op, bool) -> Result<bool, E>,
{
    /// The ops `cut` holds, the first of which names what is not
    /// stored; `None` where one of them was read from no frame.
    fn of(cut: &'a Discarded, held: H) -> Option<Self> {
        let frames: Vec<Framed> = cut.ops.iter().filter_map(|op| op.frame).collect();
        let mut stored = vec![None; frames.len()];
        *stored.first_mut()? = Some(false);
        (frames.len() == cut.ops.len()).then_some(Past {
            ops: &cut.ops,
            frames,
            stored,
            held,
            last: Vec::new(),
            looked: 1,
            keeps: None,
        })
    }

    fn unsynced(&self, at: usize) -> bool {
        self.frames[at].flags & UNSYNCED != 0
    }

    /// Whether what op `at` names is stored.
    fn stored(&mut self, at: usize) -> Result<bool, E> {
        if self.stored[at].is_none() {
            let unsynced = self.unsynced(at);
            self.stored[at] = Some((self.held)(&self.ops[at], unsynced)?);
        }
        Ok(self.stored[at] == Some(true))
    }

    /// Takes in op `at`, the ops before it taken in already; whether a
    /// sync that saw to the ref transactions up to op `at` can have
    /// ended there: the op of each workspace that last changed its
    /// index names what is stored. A sync that began past such an op
    /// did not see to it, and began where an earlier one ended, which
    /// did: where the sync began makes no difference.
    fn ends(&mut self, at: usize) -> Result<bool, E> {
        let frame = self.frames[at];
        if frame.workspace != [0; 8] && (frame.staged || frame.retired) {
            self.last
                .retain(|(workspace, _)| *workspace != frame.workspace);
            self.last.push((frame.workspace, at));
        }
        for row in 0..self.last.len() {
            let changed = self.last[row].1;
            let changes = self.frames[changed].staged && !self.frames[changed].retired;
            if changes && !self.stored(changed)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Where the first op starts, past the one refused and up to op
    /// `to`, that a writer who syncs left with what it names stored.
    fn keeps_to(&mut self, to: usize) -> Result<Option<u64>, E> {
        while self.keeps.is_none() && self.looked <= to {
            let at = self.looked;
            if !self.unsynced(at) && self.stored(at)? {
                self.keeps = Some(self.frames[at].at);
            }
            self.looked += 1;
        }
        Ok(self.keeps)
    }
}

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

    /// Where the first frame starts, past the first op of `cut`, that a
    /// sync may have covered: a writer who syncs left it, `held` finds
    /// what it names stored, and a sync can have ended at it or past
    /// it. `held` is asked of each op at most once.
    pub(super) fn kept_past<E: From<OpLogError>>(
        &self,
        cut: &Discarded,
        held: impl FnMut(&Op, bool) -> Result<bool, E>,
    ) -> Result<Option<u64>, E> {
        let Some(mut past) = Past::of(cut, held) else {
            return Ok(None);
        };
        let receipts = self.receipts_past(cut)?;
        // where the first ref transaction starts that names what is
        // not stored: no sync ended at it or past it
        let mut dangling = u64::MAX;
        for at in 0..past.frames.len() {
            let frame = past.frames[at];
            if frame.linked && !past.stored(at)? {
                dangling = frame.at;
                break;
            }
            if !past.ends(at)? {
                continue;
            }
            // a sync can have ended here, and covered all up to the
            // next op
            let next = past.frames.get(at + 1).map_or(u64::MAX, |next| next.at);
            let receipt = receipts.iter().copied().find(|receipt| *receipt < next);
            if let Some(kept) = [past.keeps_to(at)?, receipt].into_iter().flatten().min() {
                return Ok(Some(kept));
            }
        }
        // A sync that ended among what a cut past the op refused took
        // covered every frame before that cut, and saw to the ref
        // transactions among them: the last such cut before which they
        // all name what is stored.
        let cuts = self.cuts_past(cut.from)?;
        let Some(cut_past) = cuts.into_iter().find(|at| *at < dangling) else {
            return Ok(None);
        };
        let before = past.frames.iter().filter(|frame| frame.at < cut_past);
        let before = before.count();
        let receipt = receipts.iter().copied().find(|receipt| *receipt < cut_past);
        let op = past.keeps_to(before.saturating_sub(1))?;
        Ok([op, receipt].into_iter().flatten().min())
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
