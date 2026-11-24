//! Whether what an op names is stored, in a driven sequence, and what the
//! cuts a version 2 log makes and refuses for it are held to.
//!
//! The answers a look is given come from what became of the content each
//! op names (`Model::stored`): it is on disk from when the op is
//! appended, for good once a sync saw to the op, and may be gone when the
//! machine goes down before that. Nothing here follows the rule by which
//! the log decides. What it is held to is what it promises:
//!
//! - no cut takes a frame that a writer who syncs left, that a sync
//!   answered its caller for, and whose content is stored;
//! - a cut that is refused within where the log is synced is one of an
//!   op there, and one refused for a frame to keep names a frame a
//!   writer who syncs left with its content stored, past no op that
//!   every sync would have seen to and whose content is gone.
//!
//! A cut refused for a frame to keep is counted by whether a frame past
//! the op was promised to anybody, and where none was, by what the frame
//! is that the log could not tell from one: never covered by a sync,
//! covered by one that no caller was answered by, or a receipt.

use std::sync::atomic::Ordering;

use alt_oplog::{Discarded, OpLogError};

use super::v2_driver::{Run, name};
use super::v2_ledger::Entry;

impl Run {
    fn stores(&self, payload: &[u8]) -> bool {
        self.model.stored.lock().unwrap().contains(payload)
    }

    /// The content of an op another boot left past where the log is
    /// synced, which no sync saw to and nobody was told is stored since,
    /// is gone: for a look that is to find one, where there is one. The
    /// op.
    pub fn lose_one(&mut self) -> Option<Vec<u8>> {
        let (synced, boot) = (self.model.synced, self.model.boot);
        let at_risk = |op: &&Entry| {
            op.at >= synced
                && self.model.written.get(&op.payload) != Some(&boot)
                && !self.model.durable.contains(&op.payload)
                && !self.model.vouched.contains(&op.payload)
                && self.stores(&op.payload)
        };
        let at_risk: Vec<&Entry> = self.ledger.ops.iter().filter(at_risk).collect();
        if at_risk.is_empty() {
            return None;
        }
        let lost = at_risk[self.rng.below(at_risk.len() as u64) as usize]
            .payload
            .clone();
        self.model.stored.lock().unwrap().remove(&lost);
        self.trace
            .push(format!("what {} names is gone", name(&lost)));
        Some(lost)
    }

    /// The machine went down: of the ops no sync saw to, what some name
    /// is gone. One of them that a sync covered is seldom among them:
    /// whoever tells whether what ops name is stored is to vouch for an
    /// op within where the log is synced, and does not always know.
    pub fn lose_unsynced(&mut self) {
        let covered = self.model.covered;
        let mut lost = Vec::new();
        for op in &self.ledger.ops {
            let one_in = if op.end <= covered { 8 } else { 2 };
            if !self.model.durable.contains(&op.payload) && self.also.one_in(one_in) {
                lost.push(name(&op.payload));
                self.model.stored.lock().unwrap().remove(&op.payload);
            }
        }
        if !lost.is_empty() {
            self.trace.push(format!("what {lost:?} name is gone"));
        }
    }

    /// Whether op `index` of the log is one somebody was promised: a
    /// writer who syncs left it, a sync answered its caller for it, and
    /// what it names is stored.
    fn promised(&self, index: usize) -> bool {
        let op = &self.ledger.ops[index];
        !op.unsynced && self.model.answered.contains(&op.payload) && self.stores(&op.payload)
    }

    /// Whether a receipt past op `index` was promised: a writer who
    /// syncs left it, a sync answered its caller for it, and none of
    /// the ops of its request is cut with that op.
    fn receipt_promised(&self, index: usize) -> bool {
        let ops = &self.ledger.ops;
        let from = ops[index].at;
        let cut = |request: [u8; 16]| ops[index..].iter().any(|op| op.request == Some(request));
        let promised = |(_, synced, request, end): &(usize, bool, [u8; 16], u64)| {
            *end > from
                && *synced
                && self.model.answered_receipts.contains(request)
                && !cut(*request)
        };
        self.ledger.receipts.iter().any(promised)
    }

    /// The log is cut from the op of `payload` on: nothing somebody was
    /// promised is among what the cut takes past that op. The ops cut.
    pub fn cut_took(&mut self, payload: &[u8]) -> Vec<Entry> {
        let ops = &self.ledger.ops;
        let index = ops.iter().position(|op| op.payload == payload);
        let index = index.expect("the op to cut is in the log");
        if let Some(taken) = (index + 1..ops.len()).find(|index| self.promised(*index)) {
            panic!(
                "a cut from op {} on took op {}, which a writer who syncs left, a sync \
                 answered for, and whose content is stored",
                ops[index].n, ops[taken].n
            );
        }
        assert!(
            !self.receipt_promised(index),
            "a cut from op {} on took a receipt a writer who syncs left, of a request \
             whose ops are in the log, which a sync answered for",
            ops[index].n
        );
        self.ledger.cut_from(payload, self.model.synced)
    }

    /// Whether every sync that covers a frame past op `index` sees to
    /// that op: a ref transaction, or a change of an index that no op
    /// after it replaces, in a log that was not cut past it; it was
    /// last cut at `cut_at`.
    fn seen_to_by_all(&self, index: usize, cut_at: u64) -> bool {
        let ops = &self.ledger.ops;
        let op = &ops[index];
        let changes = op.staged && op.workspace.is_some() && !op.retired;
        let replaces =
            |later: &Entry| later.workspace == op.workspace && (later.staged || later.retired);
        let last = !ops[index + 1..].iter().any(replaces) && op.at >= cut_at;
        op.linked || (changes && last)
    }

    /// A writer's look came to a cut that the log refused with `why`:
    /// it was told that `refused` is not held, `asked` are the ops it
    /// asked about, and `told` what it tells as cut before. The log
    /// holds its ops as before, and the refusal is one the log was to
    /// make, or one it could not tell from that.
    pub fn cut_refused(
        &mut self,
        refused: &Option<Vec<u8>>,
        asked: &[(Vec<u8>, bool)],
        why: &OpLogError,
        told: &[Discarded],
    ) {
        self.trace.push(format!("  the cut is refused: {why:?}"));
        self.cuts_told(told, None);
        self.ledger.begun = None;
        let refused = refused.as_ref().expect("a look that refused no op cuts");
        let index = self.ledger.ops.iter().position(|op| op.payload == *refused);
        let index = index.expect("the op refused is in the log");
        let at = self.ledger.ops[index].at;
        // what it was told before that op, it was told
        let before = asked.iter().take_while(|(_, held)| *held);
        self.model
            .vouched
            .extend(before.map(|(payload, _)| payload.clone()));
        match why {
            OpLogError::Kept { unheld, kept } => {
                assert_eq!(*unheld, at, "a cut is refused at another op");
                self.kept_for(index, *kept);
            }
            OpLogError::Lost { synced, holds } => {
                assert_eq!(*holds, at, "a cut is refused at another op");
                assert!(
                    at < self.model.synced && *synced <= self.model.synced,
                    "a cut at {at} is refused as within what is synced, {synced}: the log \
                     is synced up to {}",
                    self.model.synced
                );
                self.counts.lost_refused.fetch_add(1, Ordering::Relaxed);
            }
            other => panic!("{other:?}"),
        }
    }

    /// A cut from op `index` on was refused for the frame at `kept`:
    /// that is a frame a writer who syncs left, an op whose content is
    /// stored or a receipt of a request whose ops stay, and no op from
    /// `index` up to it is one every sync would have seen to whose
    /// content is gone. Counted by whether the log had to refuse.
    fn kept_for(&mut self, index: usize, kept: u64) {
        let ops = &self.ledger.ops;
        let op = ops.iter().position(|op| op.at == kept);
        let up_to = match op {
            Some(found) => {
                assert!(
                    found > index && !ops[found].unsynced && self.stores(&ops[found].payload),
                    "a cut is refused for the frame at {kept}: no writer who syncs left it \
                     with what it names stored"
                );
                found
            }
            // a receipt: after the ops that start before it
            None => {
                let before = ops.iter().take_while(|op| op.at < kept).count();
                let from = ops[index].at;
                let cut = |of: [u8; 16]| ops[index..].iter().any(|op| op.request == Some(of));
                let mut receipts = self.ledger.receipts.iter();
                assert!(
                    kept > from
                        && receipts.any(|(_, synced, of, end)| *synced && *end > kept && !cut(*of)),
                    "a cut is refused for the frame at {kept}: no receipt a writer who \
                     syncs left lies there"
                );
                before
            }
        };
        // as far as the file tells where the log was last cut: the look
        // that was refused cut nothing
        let cut_at = self.log().cut_at;
        let gone = |at: &usize| self.seen_to_by_all(*at, cut_at) && !self.stores(&ops[*at].payload);
        if let Some(gone) = (index..up_to).find(gone) {
            panic!(
                "a cut is refused for the frame at {kept}: every sync that covered it saw \
                 to op {} first, whose content is gone",
                ops[gone].n
            );
        }
        // whether a frame past the op was promised to anybody; where
        // none was, what the frame is that could not be told from one
        let needed =
            (index + 1..ops.len()).any(|at| self.promised(at)) || self.receipt_promised(index);
        let covered = op.is_some_and(|found| ops[found].end <= self.model.covered);
        let counts = self.counts;
        let of = match (needed, op.is_some(), covered) {
            (true, ..) => &counts.kept_needed,
            (false, false, _) => &counts.kept_receipt,
            (false, true, false) => &counts.kept_uncovered,
            (false, true, true) => &counts.kept_unanswered,
        };
        of.fetch_add(1, Ordering::Relaxed);
        counts.kept_refused.fetch_add(1, Ordering::Relaxed);
    }
}
