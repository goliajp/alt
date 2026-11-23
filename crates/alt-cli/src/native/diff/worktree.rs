//! The working-tree side of `alt diff`.

use super::*;

/// What the working-tree side of a diff is read with: the index its paths
/// came from, and the attribute files that say how content is stored.
pub(super) struct Worktree {
    snapshot: alt_worktree::Snapshot,
    attrs: convert::Converter,
}

impl NativeRepo<'_> {
    /// The tracked paths whose working-tree file differs from the index:
    /// the index's entries for them, the working tree's, and what the
    /// latter are read with. No directory is listed: one stat per indexed
    /// path, and only a file whose stat changed is read. Untracked files
    /// are not in `alt diff`'s output, as they are not in git's.
    pub(super) fn worktree_sides(&self) -> Res<(Vec<WorkEntry>, Vec<WorkEntry>, Worktree)> {
        let mut snapshot = self.worktree_snapshot()?;
        let (delta, _, scanned) =
            self.scan_delta_within(&mut snapshot, alt_worktree::Untracked::No, &[])?;
        self.keep_snapshot(&mut snapshot);
        let mut old: Vec<WorkEntry> = delta
            .changed
            .iter()
            .map(|w| &w.path)
            .chain(&delta.deleted)
            .filter_map(|p| snapshot.find(p))
            .map(|i| WorkEntry {
                path: BString::from(snapshot.path(i)),
                oid: snapshot.entries[i].oid,
                mode: snapshot.entries[i].mode,
            })
            .collect();
        old.sort_by(|a, b| a.path.cmp(&b.path));
        // the converter the scan read with shows the files as well
        let attrs = match scanned {
            Some(attrs) => attrs,
            None => self.converter()?,
        };
        Ok((old, delta.changed, Worktree { snapshot, attrs }))
    }

    /// The working-tree file of `w` as it would be stored, which is how
    /// the index side it is compared with is stored.
    pub(super) fn worktree_bytes(&self, w: &WorkEntry, disk: &Worktree) -> Res<Vec<u8>> {
        let content = self.read_for(w)?;
        if w.mode == 0o120000 {
            return Ok(content);
        }
        let staged = disk
            .snapshot
            .find(&w.path)
            .map(|i| disk.snapshot.entries[i].oid);
        let stored = self.stored_form_of(&disk.attrs, staged, &w.path, &content);
        disk.attrs.check()?;
        Ok(stored.unwrap_or(content))
    }

    /// The content one side of a change is diffed as. A gitlink has no blob
    /// in this store: it diffs as the line git prints for a submodule.
    pub(super) fn side_bytes(
        &self,
        side: Option<&WorkEntry>,
        disk: Option<&Worktree>,
    ) -> Res<Vec<u8>> {
        match (side, disk) {
            (None, _) => Ok(Vec::new()),
            (Some(w), _) if w.mode == 0o160000 => {
                Ok(format!("Subproject commit {}\n", w.oid).into_bytes())
            }
            (Some(w), Some(disk)) => self.worktree_bytes(w, disk),
            (Some(w), None) => self.blob_bytes(w.oid),
        }
    }
}
