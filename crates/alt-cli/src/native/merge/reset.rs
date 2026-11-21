//! Taking back what a stopped step left in the index and the working
//! tree.

use super::*;

impl NativeRepo<'_> {
    /// Makes the index and working tree match `commit`, clearing whatever a
    /// stopped step left behind, conflicted paths included.
    pub(in crate::native) fn reset_worktree_to(&mut self, commit: ObjectId) -> Res<()> {
        let mut known: Vec<WorkEntry> = Vec::new();
        for e in &self.index()?.entries {
            if known.last().is_none_or(|k: &WorkEntry| k.path != e.path) {
                known.push(WorkEntry {
                    path: e.path.clone(),
                    oid: e.oid,
                    mode: e.mode,
                });
            }
        }
        let target = self.commit_entries(commit)?;
        self.checkout(&known, &target)
    }
}
