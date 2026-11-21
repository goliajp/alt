//! A cherry-pick that stopped on a conflict, gone on with or given up.

use super::super::commit::NewCommit;
use super::super::*;
use super::{picked_identity, report_picked};

impl NativeRepo<'_> {
    /// `alt cherry-pick --continue`: commits the resolved pick as the pick
    /// itself would have, with the author and the message of the commit
    /// picked.
    pub fn cherry_pick_continue(&mut self, json: bool, out: &mut impl Write) -> Res<()> {
        let (picked, head) = self
            .stopped_cherry_pick()?
            .ok_or("cherry-pick --continue: no cherry-pick in progress")?;
        if self.index()?.entries.iter().any(|e| e.stage() > 0) {
            return Err(
                "cherry-pick --continue: resolve the conflicts and `alt add` them first".into(),
            );
        }
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
        let (author, message) = picked_identity(&source, picked)?;
        let branch = self.head_branch()?;
        let id = self.id.clone();
        let (name, email) = id.sig();
        let committer = Sig {
            name,
            email,
            when: (now_ms() / 1000) as i64,
            tz: "+0000",
        };
        let commit = self.record_commit(
            NewCommit {
                effects: recovery::RefEffects::RefsOnly,
                tree,
                parents: &[head],
                author: &author,
                committer: &committer,
                message: &message,
            },
            &branch,
            Some(head),
            "cherry-pick",
            &Default::default(),
        )?;
        self.clear_cherry_pick()?;
        report_picked(&source, picked, commit, json, out)
    }

    /// `alt cherry-pick --abort`: the index and the files as they were
    /// before the pick, as git's `cherry-pick --abort` leaves them. The
    /// branch never moved.
    pub fn cherry_pick_abort(&mut self, out: &mut impl Write) -> Res<()> {
        let (_, head) = self
            .stopped_cherry_pick()?
            .ok_or("cherry-pick --abort: no cherry-pick in progress")?;
        self.reset_merge(head, "cherry-pick --abort")?;
        self.clear_cherry_pick()?;
        writeln!(out, "Cherry-pick aborted.")?;
        Ok(())
    }
}
