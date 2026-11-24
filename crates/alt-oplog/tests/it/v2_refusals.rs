//! The answers a driven sequence gives to whoever asks whether what an
//! op names is stored, and the cuts a version 2 log refuses for them.
//!
//! A writer told that an op is not held cuts the log there, unless that
//! would take what a sync covered. Within where the log is noted as
//! synced that is told as `Lost`. Past that, a frame after the op that a
//! writer who syncs left, with what it names stored, may have been
//! covered by a sync nothing tells of any more: the cut is refused for
//! it (`Kept`), and the log stays as it is.

use std::sync::atomic::Ordering;

use alt_oplog::{Discarded, OpLogError};

use super::v2_driver::Run;
use super::v2_ledger::Entry;

impl Run {
    /// An answer for whoever is asked whether what an op names is stored:
    /// yes to all, or to all but one op past where the log is synced;
    /// with `surely`, to all but one wherever there is one. With
    /// `within`, the one may now and then lie where the log is synced as
    /// its frames do not say yet, before an op a writer who syncs left:
    /// no cut is made there.
    pub fn refusal(&mut self, surely: bool, within: bool) -> Option<Vec<u8>> {
        let synced = self.model.synced;
        if within && self.rng.one_in(6) {
            let said = self.log().said_synced;
            let ops = &self.ledger.ops;
            let kept = |index: usize| self.to_keep(index).0.is_some();
            let inside =
                |(index, op): &(usize, &Entry)| op.at >= said && op.at < synced && kept(*index);
            let inside: Vec<&Entry> = ops
                .iter()
                .enumerate()
                .filter(inside)
                .map(|(_, op)| op)
                .collect();
            if !inside.is_empty() {
                let pick = self.rng.below(inside.len() as u64) as usize;
                return Some(inside[pick].payload.clone());
            }
        }
        let past: Vec<&Entry> = self
            .ledger
            .ops
            .iter()
            .filter(|op| op.at >= synced)
            .collect();
        match past.len() {
            0 => None,
            _ if !surely && self.rng.one_in(2) => None,
            some => Some(past[self.rng.below(some as u64) as usize].payload.clone()),
        }
    }

    /// Whether a sync that covers a frame past op `index` of the log may
    /// not have seen to that op, as the ops between the two are: for
    /// each op after it, in the order of the log. A sync sees to a ref
    /// transaction, and to a change of an index until an op of the same
    /// workspace replaces it or takes the workspace away.
    fn unseen_after(&self, index: usize) -> Vec<bool> {
        let ops = &self.ledger.ops;
        let refused = &ops[index];
        let of_workspace = refused.workspace.is_some();
        let mut unseen = !(refused.staged && of_workspace);
        let replaces = |op: &Entry| op.workspace == refused.workspace && (op.staged || op.retired);
        let mut after = Vec::new();
        for op in &ops[index + 1..] {
            unseen |= of_workspace && replaces(op);
            after.push(unseen && !refused.linked);
        }
        after
    }

    /// Where the first frame past op `index` of the log starts that a
    /// writer who syncs left and a sync may have covered without seeing
    /// to that op: an op, as far as the driver knows where, and whether
    /// a receipt of a request none of whose ops is cut with that op lies
    /// past it.
    fn to_keep(&self, index: usize) -> (Option<u64>, bool) {
        let ops = &self.ledger.ops;
        let unseen = self.unseen_after(index);
        let mut after = ops[index + 1..].iter().zip(&unseen);
        let op = after.find(|(op, unseen)| **unseen && !op.unsynced);
        let cut = |request: [u8; 16]| ops[index..].iter().any(|op| op.request == Some(request));
        // as the ops before a receipt are: those the log held when it
        // was appended
        let refused = &ops[index];
        let alone = !refused.linked && !(refused.staged && refused.workspace.is_some());
        let unseen_at = |held: usize| match held.checked_sub(index + 2) {
            Some(before) => unseen[before],
            None => alone,
        };
        let mut receipts = self.ledger.receipts.iter();
        let kept = |(after, synced, request, _): &(usize, bool, [u8; 16], u64)| {
            *after > index && *synced && !cut(*request) && unseen_at(*after)
        };
        (op.map(|(op, _)| op.at), receipts.any(kept))
    }

    /// A writer's look came to a cut that the log refused with `why`: it
    /// was told that `refused` is not held, `asked` are the ops it asked
    /// about, and `told` what it tells as cut before. The log holds its
    /// ops as before, and the refusal is one the log was to make.
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
                let (op, receipt) = self.to_keep(index);
                assert!(
                    op == Some(*kept) || (receipt && op.is_none_or(|op| *kept < op)),
                    "a cut is refused for the frame at {kept}: no writer who syncs left it"
                );
                self.counts.kept_refused.fetch_add(1, Ordering::Relaxed);
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

    /// A look cut the log from op `index` on: no frame a writer who
    /// syncs left lies past that op that a sync may have covered, op or
    /// receipt.
    pub fn cut_kept_nothing(&self, index: usize) {
        let (op, receipt) = self.to_keep(index);
        assert!(
            op.is_none(),
            "a cut took the op at {op:?}, which a writer who syncs left and a sync may \
             have covered"
        );
        assert!(
            !receipt,
            "a cut from op {} on took a receipt a writer who syncs left, of a request \
             whose ops are in the log, which a sync may have covered",
            self.ledger.ops[index].n
        );
    }
}
