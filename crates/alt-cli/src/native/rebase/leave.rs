//! Leaving a step a rebase stopped at: without it, or without the rebase.

use super::super::*;

impl NativeRepo<'_> {
    /// `alt rebase --skip`: drop the stopped step and go on.
    pub fn rebase_skip(&mut self, json: bool, out: &mut impl Write) -> Res<bool> {
        let mut state = self
            .rebase_state()?
            .ok_or("rebase --skip: no rebase in progress")?;
        // the steps that follow are committed by whoever skips this one
        self.committer()?;
        let head = self.rebase_head()?;
        (state.current, state.ran) = (None, false);
        self.save_rebase_state(&state)?;
        self.reset_worktree_to(head)?;
        self.settle_state()?;
        self.run_rebase(state, json, out)
    }

    /// `alt rebase --abort`: back to the branch as it was; the branch never
    /// moved, so only HEAD, the index and the working tree change.
    pub fn rebase_abort(&mut self, out: &mut impl Write) -> Res<()> {
        let state = self
            .rebase_state()?
            .ok_or("rebase --abort: no rebase in progress")?;
        self.clear_rebase_state()?;
        // HEAD is on the branch already where an older alt was cut short
        // between leaving the scratch ref and removing the state
        let moved = match self.head_branch()? != state.branch {
            true => self.scratch_left(&state.branch, None)?,
            false => Vec::new(),
        };
        self.reset_with(state.orig, "rebase", &moved)?;
        self.settle_state()?;
        writeln!(out, "Rebase aborted.")?;
        Ok(())
    }
}
