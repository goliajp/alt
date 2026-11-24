//! A cherry-pick that stopped on a conflict, gone on with or given up.

use super::super::commit::NewCommit;
use super::super::in_progress::{Entry, Sequence, Stands};
use super::super::*;
use super::{picked_identity, report_picked};

impl NativeRepo<'_> {
    /// `alt cherry-pick --continue`: commits the resolved pick as the pick
    /// itself would have, with the author and the message of the commit
    /// picked.
    pub fn cherry_pick_continue(&mut self, json: bool, out: &mut impl Write) -> Res<()> {
        // nobody to make the commit is told before what is left to resolve
        let committer = match self.stopped_cherry_pick()? {
            Some(_) => Some(self.committer()?),
            None => None,
        };
        let entered = self.entered_pick(Entry::Continue, out)?;
        let (Some((picked, head)), Some(committer)) = (entered, committer) else {
            return Ok(());
        };
        let noted = self.pick_noted()?;
        let head_tree = self.commit_tree(head)?;
        let tree = self.staged_tree(Some(head_tree))?;
        if tree == head_tree {
            return Err(
                "cherry-pick --continue: nothing to commit, the resolution leaves nothing \
                 of the commit; give it up with `alt cherry-pick --abort`"
                    .into(),
            );
        }
        let source = self
            .store
            .odb
            .get(&picked)?
            .ok_or("commit missing from store")?;
        let source = alt_git_codec::Commit::parse(&source.data)?;
        let (author, message) = picked_identity(&source, picked, noted)?;
        let branch = self.head_branch()?;
        self.clear_cherry_pick()?;
        let commit = self.record_commit(
            NewCommit {
                effects: recovery::RefEffects::RefsOnly,
                tree,
                parents: &[head],
                author: &author,
                committer: &committer.sig(),
                message: &message,
            },
            &branch,
            Some(head),
            "cherry-pick",
            &Default::default(),
        )?;
        report_picked(&source, picked, commit, json, out)
    }

    /// The author of the commit being picked, whom the commit that
    /// concludes the pick keeps, as git has it.
    pub(in crate::native) fn picked_author(&self) -> Res<Option<Sig<'static>>> {
        let Some((picked, _)) = self.stopped_cherry_pick()? else {
            return Ok(None);
        };
        // a commit that concludes another operation begun since is its own
        let other = self.merge_head()?.is_some() || self.revert_state()?.is_some();
        if other || self.rebase_in_progress() {
            return Ok(None);
        }
        let source = self.store.odb.get(&picked)?;
        let source = source.ok_or("commit missing from store")?;
        let source = alt_git_codec::Commit::parse(&source.data)?;
        Ok(Some(picked_identity(&source, picked, false)?.0))
    }

    /// `alt cherry-pick --abort`: the index and the files as they were
    /// before the pick, as git's `cherry-pick --abort` leaves them. The
    /// branch never moved.
    pub fn cherry_pick_abort(&mut self, out: &mut impl Write) -> Res<()> {
        let Some((_, head)) = self.entered_pick(Entry::Abort, out)? else {
            return Ok(());
        };
        self.clear_cherry_pick()?;
        self.reset_merge(head, "cherry-pick --abort", &[])?;
        self.settle_state()?;
        writeln!(out, "Cherry-pick aborted.")?;
        Ok(())
    }

    /// `alt cherry-pick --skip`: the pick that stopped is not made; the
    /// index and the files go back to the commit the branch is at, as by
    /// git's `reset --merge`.
    pub fn cherry_pick_skip(&mut self, out: &mut impl Write) -> Res<()> {
        let Some((_, head)) = self.entered_pick(Entry::Skip, out)? else {
            return Ok(());
        };
        self.clear_cherry_pick()?;
        self.reset_merge(head, "cherry-pick --skip", &[])?;
        self.settle_state()
    }

    /// `alt cherry-pick --quit`: the pick is forgotten; the index and
    /// the files stay as they are.
    pub fn cherry_pick_quit(&mut self, out: &mut impl Write) -> Res<()> {
        if self.entered_pick(Entry::Quit, out)?.is_some() {
            self.clear_cherry_pick()?;
            self.settle_state()?;
        }
        Ok(())
    }

    /// The pick that stopped, the commit picked and the commit HEAD is
    /// at, once `entry` may go back into it.
    fn entered_pick(
        &self,
        entry: Entry,
        out: &mut impl Write,
    ) -> Res<Option<(ObjectId, ObjectId)>> {
        let stopped = self.stopped_cherry_pick()?;
        let stands = match stopped {
            Some(_) => Stands::Open,
            None => Stands::Nowhere,
        };
        let entered = self.enter(Sequence::CherryPick, entry, stands, out)?;
        Ok(entered.and(stopped))
    }
}
