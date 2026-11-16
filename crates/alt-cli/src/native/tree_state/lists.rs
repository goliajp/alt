//! What a command prints of the three trees compared, and whether they
//! leave a checkout free to write.

use alt_worktree::Staged;

use super::super::*;
use super::TreeState;

impl TreeState {
    /// The three lists `status` prints.
    pub(in crate::native) fn status(&self) -> alt_worktree::Status {
        let kind = |s: &Staged| match (&s.old, &s.new) {
            (None, _) => ChangeKind::Added,
            (_, None) => ChangeKind::Deleted,
            (Some(old), Some(new)) => ChangeKind::between(old.mode, new.mode),
        };
        let snapshot = &self.snapshot;
        let changed = |w: &WorkEntry| {
            let staged = snapshot.find(&w.path).map(|at| snapshot.entries[at].mode);
            let kind = staged.map_or(ChangeKind::Modified, |mode| {
                ChangeKind::between(mode, w.mode)
            });
            (w.path.clone(), kind)
        };
        let gone = |path: &BString| (path.clone(), ChangeKind::Deleted);
        let mut unstaged: Vec<(BString, ChangeKind)> = self
            .delta
            .changed
            .iter()
            .map(changed)
            .chain(self.delta.deleted.iter().map(gone))
            .collect();
        unstaged.sort_by(|a, b| a.0.cmp(&b.0));
        alt_worktree::Status {
            staged: self
                .staged
                .iter()
                .map(|s| (s.path.clone(), kind(s)))
                .collect(),
            unstaged,
            untracked: self.delta.untracked.clone(),
        }
    }

    /// The paths with a conflict in the index.
    pub(in crate::native) fn unmerged(&self) -> std::collections::BTreeSet<BString> {
        let s = &self.snapshot;
        (0..s.entries.len())
            .filter(|&i| s.entries[i].stage() > 0)
            .map(|i| BString::from(s.path(i)))
            .collect()
    }
}

impl NativeRepo<'_> {
    /// Refuses `action` if any tracked file has staged or unstaged changes, so
    /// a tree-rewriting operation never clobbers uncommitted work.
    pub(in crate::native) fn ensure_clean(&self, action: &str) -> Res<()> {
        let state = self.tree_state(alt_worktree::Untracked::No)?;
        // a submodule that checked out another commit is not overwritten:
        // a checkout never touches what is inside it
        let changed = state.delta.changed.iter().any(|w| w.mode != 0o160000);
        // nor is the missing directory of one anything it would write over
        let snapshot = &state.snapshot;
        let submodule = |path: &BString| {
            let entry = snapshot.find(path).map(|at| &snapshot.entries[at]);
            entry.is_some_and(|e| e.mode == 0o160000)
        };
        let deleted = state.delta.deleted.iter().any(|path| !submodule(path));
        if !state.staged.is_empty() || changed || deleted {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                format!("your local changes would be overwritten by {action}; commit them first"),
            )
            .into());
        }
        Ok(())
    }
}
