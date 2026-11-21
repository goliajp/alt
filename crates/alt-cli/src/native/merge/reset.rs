//! Taking back what a stopped merge, pick or revert left in the index and
//! the working tree.

use super::*;

impl NativeRepo<'_> {
    /// Makes the index and working tree match `commit`, clearing whatever a
    /// stopped step left behind, conflicted paths included.
    pub(in crate::native) fn reset_worktree_to(&mut self, commit: ObjectId) -> Res<()> {
        self.reset_with(commit, "reset", &[])
    }

    /// [`reset_worktree_to`](Self::reset_worktree_to) together with the ref
    /// changes `moved`, where there are any, in one ref transaction as
    /// `verb`.
    pub(in crate::native) fn reset_with(
        &mut self,
        commit: ObjectId,
        verb: &str,
        moved: &[RefChange],
    ) -> Res<()> {
        let target = self.commit_entries(commit)?;
        let wanted: std::collections::HashMap<&BString, &WorkEntry> =
            target.iter().map(|t| (&t.path, t)).collect();
        let mut known: Vec<WorkEntry> = Vec::new();
        for e in &self.index()?.entries {
            let entry = WorkEntry {
                path: e.path.clone(),
                oid: e.oid,
                mode: e.mode,
            };
            match known.last_mut() {
                // a path in conflict stands as a stage `commit` does not
                // have, so that its file is written again: the file holds
                // the conflict whatever stage comes first
                Some(last) if last.path == e.path => {
                    let wanted = wanted.get(&e.path);
                    if wanted.is_none_or(|t| t.oid != e.oid || t.mode != e.mode) {
                        *last = entry;
                    }
                }
                _ => known.push(entry),
            }
        }
        if moved.is_empty() {
            return self.checkout(&known, &target);
        }
        self.commit_refs_checkout(verb, moved, &known, &target)?;
        Ok(())
    }

    /// git's `reset --merge` to `commit`, which a merge or a pick that is
    /// given up ends with: [`reset_worktree_to`](Self::reset_worktree_to),
    /// refused before anything is written where it would write over a
    /// change that is not staged. A file the index has as `commit` has it
    /// is left alone, changed or not. The ref changes `moved` are made with
    /// it, in one ref transaction.
    pub(in crate::native) fn reset_merge(
        &mut self,
        commit: ObjectId,
        verb: &str,
        moved: &[RefChange],
    ) -> Res<()> {
        let target = self.commit_entries(commit)?;
        let wanted: std::collections::HashMap<&BString, &WorkEntry> =
            target.iter().map(|t| (&t.path, t)).collect();
        let state = self.tree_state(alt_worktree::Untracked::No)?;
        // what a submodule has checked out is nothing a checkout writes over
        let changed = state.delta.changed.iter().filter(|w| w.mode != 0o160000);
        let changed: std::collections::HashSet<&BString> = changed.map(|w| &w.path).collect();
        for e in self.index()?.entries.iter().filter(|e| e.stage() == 0) {
            let kept = wanted
                .get(&e.path)
                .is_some_and(|t| t.oid == e.oid && t.mode == e.mode);
            if !kept && changed.contains(&e.path) {
                return Err(crate::machine::Failure::refusal(
                    "precondition_failed",
                    format!(
                        "{verb}: '{}' has changes that are not staged and would be \
                         overwritten; stage them or take them back first",
                        e.path
                    ),
                )
                .into());
            }
        }
        drop(state);
        let verb = verb.split(' ').next().unwrap_or(verb);
        self.reset_with(commit, verb, moved)
    }
}
