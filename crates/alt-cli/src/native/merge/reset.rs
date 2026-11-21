//! Taking back what a stopped step left in the index and the working
//! tree.

use super::*;

impl NativeRepo<'_> {
    /// Makes the index and working tree match `commit`, clearing whatever a
    /// stopped step left behind, conflicted paths included.
    pub(in crate::native) fn reset_worktree_to(&mut self, commit: ObjectId) -> Res<()> {
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
        self.checkout(&known, &target)
    }
}
