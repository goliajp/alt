//! What a merge that stopped in conflict leaves behind, and giving the
//! merge up.
//!
//! `MERGE_HEAD` lies next to the index of the workspace and names the
//! commit being merged in. The commit that concludes the merge removes
//! it, and so does `alt merge --abort`.

use super::in_progress::{Entry, Sequence, Stands};
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

    /// Records `theirs` as the second parent the resolving commit will
    /// get, with the conflict the merge is about to write.
    pub(in crate::native) fn start_merge(&self, theirs: ObjectId) -> Res<()> {
        self.plan_state("MERGE_HEAD", Some(format!("{theirs}\n")))
    }

    /// Forgets the unfinished merge with the commit that concludes it or
    /// the reset that gives it up.
    pub(in crate::native) fn finish_merge(&self) -> Res<()> {
        self.plan_state("MERGE_HEAD", None)
    }

    /// Refuses `verb` while a merge, bisect, rebase or revert waits to be
    /// finished.
    pub(in crate::native) fn ensure_no_merge_in_progress(&self, verb: &str) -> Res<()> {
        use in_progress::OperationKind;
        let hint = match self.operation_kind()? {
            None | Some(OperationKind::CherryPick) => return Ok(()),
            Some(OperationKind::Merge) => {
                "a merge is in progress; resolve the conflicts and commit it first, \
                 or give it up with `alt merge --abort`"
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

    fn merge_message_path(&self) -> PathBuf {
        self.index_path.with_file_name("MERGE_MSG")
    }

    /// Keeps the message the commit of the merge of `theirs` is to
    /// have, for `alt merge --continue`.
    pub(in crate::native) fn keep_merge_message(&self, theirs: ObjectId, message: &str) -> Res<()> {
        let text = format!("{theirs}\n{message}");
        Ok(crate::durable::write(
            self.merge_message_path(),
            text.as_bytes(),
        )?)
    }

    /// The message kept for the merge of `theirs`; where an older alt
    /// began the merge and kept none, git's for a commit without a name.
    fn kept_merge_message(&self, theirs: ObjectId) -> Res<String> {
        let kept = match std::fs::read_to_string(self.merge_message_path()) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        let of = format!("{theirs}\n");
        Ok(match kept.strip_prefix(&of) {
            Some(message) => message.to_owned(),
            None => format!("Merge commit '{theirs}'\n"),
        })
    }

    fn merge_stands(&self) -> Res<(Stands, Option<ObjectId>)> {
        let theirs = self.merge_head()?;
        Ok(match theirs {
            Some(_) => (Stands::Open, theirs),
            None => (Stands::Nowhere, None),
        })
    }

    /// `alt merge --continue`: the commit that concludes the merge, with
    /// the message the merge would have given it.
    pub fn merge_continue(&mut self, json: bool, out: &mut impl Write) -> Res<()> {
        let (stands, theirs) = self.merge_stands()?;
        let entered = self.enter(Sequence::Merge, Entry::Continue, stands, out)?;
        let (Some(_), Some(theirs)) = (entered, theirs) else {
            return Ok(());
        };
        let message = self.kept_merge_message(theirs)?;
        self.commit(&message, Default::default(), json, out)
    }

    /// `alt merge --quit`: the merge is forgotten; the index and the
    /// files stay as they are.
    pub fn merge_quit(&mut self, out: &mut impl Write) -> Res<()> {
        let (stands, _) = self.merge_stands()?;
        if self
            .enter(Sequence::Merge, Entry::Quit, stands, out)?
            .is_some()
        {
            self.finish_merge()?;
            self.settle_state()?;
        }
        Ok(())
    }

    /// `alt merge --abort`: the index and the files as they were before
    /// the merge, as git's `merge --abort` leaves them. The branch never
    /// moved.
    pub fn merge_abort(&mut self, out: &mut impl Write) -> Res<()> {
        let (stands, _) = self.merge_stands()?;
        if self
            .enter(Sequence::Merge, Entry::Abort, stands, out)?
            .is_none()
        {
            return Ok(());
        }
        let head = self
            .store
            .refs
            .resolve(&self.head_ref)?
            .ok_or("merge --abort: the branch has no commits yet")?;
        self.finish_merge()?;
        self.reset_merge(head, "merge --abort", &[])?;
        self.settle_state()?;
        writeln!(out, "Merge aborted.")?;
        Ok(())
    }
}
