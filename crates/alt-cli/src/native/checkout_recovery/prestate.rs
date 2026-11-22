//! The tracked files and the index before an operation of several steps.

use super::super::*;
use super::WorkspaceSnapshot;
use recovery::canonical_index;

impl NativeRepo<'_> {
    pub(in crate::native) fn snapshot_workspace(&self) -> Res<String> {
        let index = self.index()?;
        let mut files = Vec::new();
        for entry in &index.entries {
            let state = self.observed(&entry.path, true, entry.mode == 0o160000, &|_| false)?;
            files.push((entry.path.to_vec(), state));
        }
        let snapshot = WorkspaceSnapshot {
            version: 1,
            workspace: self.workspace_identity()?,
            index: self.snapshot(&canonical_index(index, self.store.algo()?))?,
            files,
        };
        self.snapshot(&alt_json::to_vec(&snapshot)?)
    }
}
