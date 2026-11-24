//! Whether what an op names is stored, in a driven sequence, and what the
//! cuts a version 2 log makes and refuses for it are held to.
//!
//! The answers a look is given come from what became of the content each
//! op names (`Model::stored`): it is on disk from when the op is
//! appended, for good once a sync saw to the op, and may be gone when the
//! machine goes down before that. What the log is held to:
//!
//! - no cut takes a frame that a writer who syncs left, that the log
//!   was recorded as synced over, and whose content is stored;
//! - a cut is refused for a frame to keep where a sync can have covered
//!   such a frame past the op refused, and made where none can
//!   (`v2_coverable`): the log neither keeps what no sync covered nor
//!   cuts what one may have;
//! - a cut that is refused within where the log is synced is one of an
//!   op there.
//!
//! A cut refused for a frame to keep is counted by whether the log was
//! ever recorded as synced over a frame past the op that is one to
//! keep: where it was, the log had to refuse; where it was not, a sync
//! can have covered one, and the log cannot tell that none did.

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
        let mut lost = Vec::new();
        for op in &self.ledger.ops {
            let covered = self.model.covered.contains(&op.payload);
            let one_in = if covered { 8 } else { 2 };
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
    /// writer who syncs left it, the log was recorded as synced over
    /// it, and what it names is stored.
    fn promised(&self, index: usize) -> bool {
        let op = &self.ledger.ops[index];
        !op.unsynced && self.model.covered.contains(&op.payload) && self.stores(&op.payload)
    }

    /// Whether a receipt past op `index` was promised: a writer who
    /// syncs left it, the log was recorded as synced over it, and none
    /// of the ops of its request is cut with that op.
    fn receipt_promised(&self, index: usize) -> bool {
        let ops = &self.ledger.ops;
        let from = ops[index].at;
        let cut = |request: [u8; 16]| ops[index..].iter().any(|op| op.request == Some(request));
        let promised = |(_, synced, request, end): &(usize, bool, [u8; 16], u64)| {
            *end > from
                && *synced
                && self.model.covered_receipts.contains(request)
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
                 covered, and whose content is stored",
                ops[index].n, ops[taken].n
            );
        }
        assert!(
            !self.receipt_promised(index),
            "a cut from op {} on took a receipt a writer who syncs left, of a request \
             whose ops are in the log, which a sync covered",
            ops[index].n
        );
        // the cut is made by now: where the log was last cut before it
        assert!(
            self.to_keep(index, &self.cuts_seen) == (None, None),
            "a cut from op {} on took a frame a sync can have covered",
            ops[index].n
        );
        self.ledger.cut_from(payload, self.model.synced)
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

    /// A cut from op `index` on was refused for the frame at `kept`: a
    /// sync can have covered a frame past that op which a writer who
    /// syncs left with what it names stored, and `kept` is the first
    /// such frame. Counted by whether the log had to refuse.
    fn kept_for(&self, index: usize, kept: u64) {
        let ops = &self.ledger.ops;
        // the look that was refused cut nothing
        let found = match self.to_keep(index, &self.log().cuts) {
            _ if self.looks_behind => true,
            (Some(op), receipt) if receipt.is_none_or(|end| op < end) => kept == op,
            (_, Some(end)) => kept < end && ops.iter().all(|op| op.at != kept),
            (_, None) => false,
        };
        assert!(
            found,
            "a cut is refused for the frame at {kept}: it is not the first frame past op {} \
             that a sync can have covered and a writer who syncs left with what it names stored",
            ops[index].n
        );
        self.kept_counted(index);
    }

    /// Counts a cut from op `index` on that was refused for a frame to
    /// keep, by whether the log was recorded as synced over one.
    fn kept_counted(&self, index: usize) {
        let ops = &self.ledger.ops;
        let needed =
            (index + 1..ops.len()).any(|at| self.promised(at)) || self.receipt_promised(index);
        let of = match needed {
            true => &self.counts.kept_needed,
            false => &self.counts.kept_uncovered,
        };
        of.fetch_add(1, Ordering::Relaxed);
        self.counts.kept_refused.fetch_add(1, Ordering::Relaxed);
    }
}
