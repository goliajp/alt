//! Going back into a rebase that stopped: on with it, without the step
//! it stopped at, or out of it.

use super::super::in_progress::{Entry, Sequence, Stands};
use super::super::sequence_editor::Action;
use super::super::*;
use super::RebaseState;

impl NativeRepo<'_> {
    /// The rebase under way with what it remembers, once `entry` may go
    /// back into it; `None` where it has said why it may not.
    fn entered_rebase(
        &self,
        entry: Entry,
        out: &mut impl Write,
    ) -> Res<Option<(RebaseState, bool)>> {
        let state = self.rebase_state()?;
        let stands = match &state {
            None => Stands::Nowhere,
            Some(state) if state.current.is_some() || state.merging.is_some() => Stands::Open,
            Some(_) => Stands::Concluded,
        };
        let waiting = self.enter(Sequence::Rebase, entry, stands, out)?;
        Ok(waiting.zip(state).map(|(w, state)| (state, w.staged)))
    }

    /// `alt rebase --continue`: commit the resolved step, then go on.
    /// What was staged after a commit of the user concluded the step is
    /// committed as git commits it: as the step again.
    pub fn rebase_continue(&mut self, json: bool, out: &mut impl Write) -> Res<bool> {
        if let Some(state) = self.rebase_state()? {
            self.committer()?;
            // stopped for a shell command: what it left is said of it
            if state.ran {
                self.ensure_left_clean()?;
            }
        }
        let Some((mut state, staged)) = self.entered_rebase(Entry::Continue, out)? else {
            return Ok(true);
        };
        let head_tree = self.commit_tree(self.rebase_head()?)?;
        if let Some((action, commit)) = state.current {
            let tree = self.staged_tree(Some(head_tree))?;
            if tree == head_tree && !action.melds() {
                return Err(
                    "rebase --continue: no changes staged; if nothing is left of \
                            this commit, run `alt rebase --skip`"
                        .into(),
                );
            }
            state.current = None;
            self.save_rebase_state(&state)?;
            self.apply_step(action, commit, tree, state.fresh_dates)?;
        } else if let Some((commit, theirs)) = state.merging {
            let tree = self.staged_tree(Some(head_tree))?;
            state.merging = None;
            self.save_rebase_state(&state)?;
            self.record_merge_again(commit, theirs, tree, state.fresh_dates)?;
        } else if let Some((_, commit)) = state.concluded
            && staged
        {
            let tree = self.staged_tree(Some(head_tree))?;
            state.concluded = None;
            self.save_rebase_state(&state)?;
            self.apply_step(Action::Pick, commit, tree, state.fresh_dates)?;
        }
        // what a step that follows stages is not taken for what was
        // staged after the commit of the user, should it be cut short
        if state.concluded.take().is_some() {
            self.save_rebase_state(&state)?;
        }
        state.ran = false;
        self.run_rebase(state, json, out)
    }

    /// `alt rebase --skip`: drop the stopped step and go on. Git resets
    /// hard here and loses what is not staged; alt refuses where a file
    /// that is not in conflict holds such a change.
    pub fn rebase_skip(&mut self, json: bool, out: &mut impl Write) -> Res<bool> {
        // the steps that follow are committed by whoever skips this one
        if self.rebase_in_progress() {
            self.committer()?;
        }
        let Some((mut state, _)) = self.entered_rebase(Entry::Skip, out)? else {
            return Ok(true);
        };
        let head = self.rebase_head()?;
        (state.current, state.merging, state.ran) = (None, None, false);
        state.concluded = None;
        self.save_rebase_state(&state)?;
        self.reset_worktree_to(head)?;
        self.settle_state()?;
        self.run_rebase(state, json, out)
    }

    /// `alt rebase --abort`: back to the branch as it was; the branch never
    /// moved, so only HEAD, the index and the working tree change, as by
    /// the hard reset of git: a change that is not committed is gone,
    /// staged or not, and an undo brings it back.
    pub fn rebase_abort(&mut self, out: &mut impl Write) -> Res<()> {
        let Some((state, _)) = self.entered_rebase(Entry::Abort, out)? else {
            return Ok(());
        };
        self.clear_rebase_state()?;
        // HEAD is on the branch already where an older alt was cut short
        // between leaving the scratch ref and removing the state
        let moved = match self.head_branch()? != state.branch {
            true => self.scratch_left(&state.branch, None)?,
            false => Vec::new(),
        };
        self.reset_hard_with(state.orig, "rebase", &moved)?;
        self.settle_state()?;
        writeln!(out, "Rebase aborted.")?;
        Ok(())
    }

    /// `alt rebase --quit`: the rebase is forgotten, with the index and
    /// the files as they are. Git leaves HEAD at the commits made so
    /// far, on no branch; a store has no such HEAD, so HEAD goes back to
    /// the branch, which never moved, and the commits made are named.
    pub fn rebase_quit(&mut self, out: &mut impl Write) -> Res<()> {
        let Some((state, _)) = self.entered_rebase(Entry::Quit, out)? else {
            return Ok(());
        };
        let made = self.rebase_head()?;
        let moved = self.scratch_left(&state.branch, None)?;
        self.clear_rebase_state()?;
        self.commit_refs("rebase", &moved)?;
        let branch = &state.branch;
        let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
        writeln!(
            out,
            "The rebase is forgotten: HEAD is back on {short}, which did not move, and the \
             index and the files are as the rebase left them.\nThe commits it made end at \
             {made}; `alt branch <name> {made}` keeps them on a branch."
        )?;
        if self.index_apart_from(state.orig)? {
            writeln!(
                out,
                "The index is not that of {short}: it holds what the rebase made of the commits \
                 it went through and nothing of those it had yet to make, so a commit made now \
                 takes what they changed out of {short} again.\n`alt reset` puts the index back \
                 to {short} and leaves the files; `alt reset --hard` puts back the files as well."
            )?;
        }
        Ok(())
    }

    /// Whether the index holds anything but the files of `commit`.
    fn index_apart_from(&self, commit: ObjectId) -> Res<bool> {
        let held = self.commit_entries(commit)?;
        let index = self.index()?;
        let same = |(staged, held): (&IndexEntry, &WorkEntry)| {
            staged.stage() == 0
                && (&staged.path, staged.oid, staged.mode) == (&held.path, held.oid, held.mode)
        };
        Ok(index.entries.len() != held.len() || !index.entries.iter().zip(&held).all(same))
    }
}
