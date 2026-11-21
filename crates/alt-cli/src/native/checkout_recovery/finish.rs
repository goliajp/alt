//! A checkout that was cut short, finished from its intent.

use super::super::*;
use super::{CheckoutIntent, FileChange};
use recovery::{canonical_index, durable_write, hash};

fn refusal(why: impl Into<String>) -> Box<dyn std::error::Error> {
    crate::machine::Failure::refusal("precondition_failed", why).into()
}

impl NativeRepo<'_> {
    /// Makes what is left to make of the checkout `operation` is the intent
    /// of. Each path must be as the intent found it or as it leaves it.
    /// `of_step`, a path that is neither is not always in the way: see
    /// [`left_to_write`](Self::left_to_write).
    pub(in crate::native) fn finish_interrupted_checkout(
        &mut self,
        operation: OpId,
        intent: CheckoutIntent,
        of_step: bool,
    ) -> Res<()> {
        if intent.version != 1 || intent.workspace != self.workspace_identity()? {
            return Err(refusal("recovery workspace instance does not match"));
        }
        let before = self.read_snapshot(&intent.index_before)?;
        let after = self.read_snapshot(&intent.index_after)?;
        let algo = self.store.algo()?;
        Index::parse(&before, algo)?;
        let index_after = Index::parse(&after, algo)?;
        // every path and snapshot is checked before the first byte changes
        let outstanding = self.left_to_write(&intent, of_step.then_some(&index_after))?;
        let current = hash(&canonical_index(self.index()?, algo));
        if current != intent.index_before && current != intent.index_after {
            return Err(refusal(
                "recovery precondition failed: index matches neither recorded state",
            ));
        }
        self.ensure_directories_replaceable(&intent)?;
        self.apply_changes(outstanding.iter().copied())?;
        if current != intent.index_after {
            durable_write(&self.index_path, &after)?;
        }
        self.snapshot_checked_out(&after)?;
        self.record_checkout_done("recover", operation)
    }

    /// The changes of `intent` that are not made yet. A path that is
    /// neither as the intent found it nor as it leaves it is refused, and
    /// named. Where the checkout is one of a step, `conflicts` is the index
    /// it leaves, and such a path is passed over where that index has it in
    /// conflict: what is there is what the user made of the conflict, which
    /// the operation under way leaves to them. One that is gone is written.
    fn left_to_write<'c>(
        &self,
        intent: &'c CheckoutIntent,
        conflicts: Option<&Index>,
    ) -> Res<Vec<&'c FileChange>> {
        let mut outstanding = Vec::new();
        let paths: std::collections::HashSet<&[u8]> =
            intent.files.iter().map(|c| c.path.as_slice()).collect();
        for change in &intent.files {
            let path = BString::from(change.path.clone());
            self.ensure_path_allowed(&path)?;
            for state in [&change.before, &change.after].into_iter().flatten() {
                if state.mode != 0o160000 {
                    self.read_snapshot(&state.contents)?;
                }
            }
            let actual = self.observed_among(&paths, change)?;
            // a path that changes between a submodule directory and a file
            // is absent for a moment: the one goes before the other comes
            let kinds =
                [&change.before, &change.after].map(|s| s.as_ref().map(|s| s.mode == 0o160000));
            let between = actual.is_none() && matches!(kinds, [Some(a), Some(b)] if a != b);
            if actual != change.before && actual != change.after && !between {
                let Some(index) = conflicts else {
                    return Err(refusal(format!(
                        "recovery precondition failed: '{path}' matches neither recorded state"
                    )));
                };
                let in_conflict = |e: &IndexEntry| e.stage() > 0 && e.path == path;
                if actual.is_some() && index.entries.iter().any(in_conflict) {
                    continue;
                }
                if actual.is_some() {
                    return Err(refusal(format!(
                        "'{path}' was changed after the operation was interrupted, and the \
                         operation has other content to put there"
                    )));
                }
            }
            if actual != change.after {
                outstanding.push(change);
            }
        }
        Ok(outstanding)
    }
}
