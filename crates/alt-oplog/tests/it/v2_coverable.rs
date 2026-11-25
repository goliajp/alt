//! Whether a sync can have covered a frame past an op that a look
//! refused, told from what the driver knows of the ops of a driven
//! version 2 log and of what became of what they name: for holding the
//! cuts the log makes and refuses to the rule it gives for them, by
//! other means than the log has.
//!
//! A sync ends after an op. It began at the op refused or before, or
//! where a sync that can have been made ended. It saw to every ref
//! transaction it covered, and to the op of each workspace that last
//! touched its index as the log stood where the sync ended, where that
//! op lies within what it covered: all of them name what is stored.
//! Every pair of where a sync began and ended is tried.

use super::v2_driver::Run;
use super::v2_ledger::Entry;

impl Run {
    fn stored(&self, op: &Entry) -> bool {
        self.model.stored.lock().unwrap().contains(&op.payload)
    }

    /// Whether a sync that covered the ops `from..=to` of the log saw to
    /// nothing but ops that name what is stored.
    fn saw_to_what_is_stored(&self, from: usize, to: usize) -> bool {
        let ops = &self.ledger.ops;
        (from..=to).all(|at| {
            let op = &ops[at];
            let touches =
                |later: &Entry| later.workspace == op.workspace && (later.staged || later.retired);
            let last = !ops[at + 1..=to].iter().any(touches);
            let changes = op.workspace.is_some() && op.staged && !op.retired;
            !(op.linked || (changes && last)) || self.stored(op)
        })
    }

    /// After which ops, from op `refused` on, a sync can have ended.
    fn ends_of_a_sync(&self, refused: usize) -> Vec<usize> {
        let mut ends: Vec<usize> = Vec::new();
        for to in refused..self.ledger.ops.len() {
            let begun = std::iter::once(refused).chain(ends.iter().map(|end| end + 1));
            let begun: Vec<usize> = begun.collect();
            if begun
                .into_iter()
                .any(|from| self.saw_to_what_is_stored(from, to))
            {
                ends.push(to);
            }
        }
        ends
    }

    /// The first frames past op `refused` that a writer who syncs left
    /// with what they name stored, among the ops up to op `to` and the
    /// receipts before the op after that end by byte `by`: where the
    /// op starts, and where the receipt ends.
    fn first_to_keep(&self, refused: usize, to: usize, by: u64) -> (Option<u64>, Option<u64>) {
        let ops = &self.ledger.ops;
        let kept = |op: &&Entry| !op.unsynced && self.stored(op);
        let op = ops[refused + 1..=to].iter().find(kept).map(|op| op.at);
        let cut = |of: [u8; 16]| ops[refused..].iter().any(|op| op.request == Some(of));
        let from = ops[refused].at;
        let receipts = self.ledger.receipts.iter();
        let mut receipts = receipts.filter(|(after, synced, of, end)| {
            *after <= to + 1 && *synced && *end > from && *end <= by && !cut(*of)
        });
        (op, receipts.next().map(|(.., end)| *end))
    }

    /// The first frames past op `refused` that are to be kept from a cut
    /// of the log, which was cut of ops at `cuts`: where the first op
    /// starts, and where the first receipt ends, that a sync can have
    /// covered and a writer who syncs left with what they name stored.
    /// A sync that ended among what one of those cuts took covered
    /// every frame before the cut, and what it saw to there nobody
    /// knows but for the ref transactions that are left.
    pub fn to_keep(&self, refused: usize, cuts: &[u64]) -> (Option<u64>, Option<u64>) {
        let ops = &self.ledger.ops;
        let ends = self.ends_of_a_sync(refused);
        let mut kept = match ends.last() {
            Some(to) => self.first_to_keep(refused, *to, u64::MAX),
            None => (None, None),
        };
        let first = |one: Option<u64>, other: Option<u64>| [one, other].into_iter().flatten().min();
        for cut in cuts {
            let before = ops.iter().take_while(|op| op.at < *cut).count();
            let began = ends.iter().map(|end| end + 1);
            let mut began = std::iter::once(refused).chain(began.filter(|from| *from <= before));
            let linked_stored =
                |from: usize| (from..before).all(|at| !ops[at].linked || self.stored(&ops[at]));
            if before > refused && began.any(linked_stored) {
                let (op, receipt) = self.first_to_keep(refused, before - 1, *cut);
                kept = (first(kept.0, op), first(kept.1, receipt));
            }
        }
        kept
    }
}
