//! What a command prints of the three trees compared.

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
