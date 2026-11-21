//! What a merge that stopped in conflict leaves behind.
//!
//! `MERGE_HEAD` lies next to the index of the workspace and names the
//! commit being merged in. The commit that concludes the merge removes
//! it.

use super::*;

impl NativeRepo<'_> {
    /// Where an unfinished merge records the commit being merged in, next to
    /// this workspace's index, so each workspace has its own.
    fn merge_head_path(&self) -> PathBuf {
        self.index_path.with_file_name("MERGE_HEAD")
    }

    /// The commit an unfinished (conflicted) merge is bringing in, if any.
    pub(in crate::native) fn merge_head(&self) -> Res<Option<ObjectId>> {
        match std::fs::read_to_string(self.merge_head_path()) {
            Ok(s) => Ok(Some(s.trim().parse().map_err(|_| "corrupt MERGE_HEAD")?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Records `theirs` as the second parent the resolving commit will get.
    pub(in crate::native) fn start_merge(&self, theirs: ObjectId) -> Res<()> {
        crate::durable::write(self.merge_head_path(), format!("{theirs}\n"))?;
        Ok(())
    }

    /// Forgets the unfinished merge once its commit is recorded.
    pub(in crate::native) fn finish_merge(&self) -> Res<()> {
        match crate::durable::remove_file(self.merge_head_path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// Refuses `verb` while a merge, bisect, rebase or revert waits to be
    /// finished.
    pub(in crate::native) fn ensure_no_merge_in_progress(&self, verb: &str) -> Res<()> {
        use in_progress::OperationKind;
        let hint = match self.operation_kind()? {
            None | Some(OperationKind::CherryPick) => return Ok(()),
            Some(OperationKind::Merge) => {
                "a merge is in progress; resolve the conflicts and commit it first"
            }
            Some(OperationKind::Bisect) => {
                "a bisect is in progress; end it with `alt bisect reset`"
            }
            Some(OperationKind::Rebase) => {
                "a rebase is in progress; finish it with `alt rebase --continue`, \
                 `--skip` or `--abort`"
            }
            Some(OperationKind::Revert) => {
                "a revert is in progress; finish it with `alt revert --continue` \
                 or `alt revert --abort`"
            }
        };
        Err(format!("{verb}: {hint}").into())
    }
}
