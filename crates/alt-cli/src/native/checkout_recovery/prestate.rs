//! The tracked files and the index before an operation of several steps,
//! and what the steps changed of them in all.

use super::super::*;
use super::{CheckoutIntent, FileChange, WorkspaceSnapshot};
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

    pub(in crate::native) fn completed_workspace_delta(
        &self,
        snapshot: &str,
    ) -> Res<CheckoutIntent> {
        let snapshot: WorkspaceSnapshot = alt_json::from_slice(&self.read_snapshot(snapshot)?)?;
        if snapshot.version != 1 || snapshot.workspace != self.workspace_identity()? {
            return Err("invalid workspace recovery snapshot".into());
        }
        Index::parse(&self.read_snapshot(&snapshot.index)?, self.store.algo()?)?;
        let index = self.index()?;
        let before: std::collections::BTreeMap<_, _> = snapshot.files.into_iter().collect();
        let paths: std::collections::BTreeSet<_> = before
            .keys()
            .cloned()
            .chain(index.entries.iter().map(|e| e.path.to_vec()))
            .collect();
        let gitlinks: std::collections::HashSet<&[u8]> = index
            .entries
            .iter()
            .filter(|e| e.mode == 0o160000)
            .map(|e| e.path.as_slice())
            .collect();
        let mut files = Vec::new();
        for path in &paths {
            let path_bytes = BString::from(path.clone());
            let before = before.get(path).cloned().flatten();
            let gitlink = gitlinks.contains(path.as_slice())
                || before.as_ref().is_some_and(|s| s.mode == 0o160000);
            let after = self.observed(&path_bytes, true, gitlink, &|p| paths.contains(p))?;
            let path = path.clone();
            if before != after {
                files.push(FileChange {
                    path,
                    before,
                    after,
                });
            }
        }
        Ok(CheckoutIntent {
            version: 1,
            workspace: snapshot.workspace,
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            index_before: snapshot.index,
            index_after: self.snapshot(&canonical_index(index, self.store.algo()?))?,
            files,
        })
    }
}
