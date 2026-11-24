//! Leaving a step a rebase stopped at: without it, or without the rebase.

use std::collections::HashSet;

use super::super::*;

impl NativeRepo<'_> {
    /// Before `alt rebase --continue` commits a step that stopped on a
    /// conflict: every conflict is resolved. Whether it goes on: as git
    /// has it, not while a tracked file holds a change that is not
    /// staged, which is said as git says it.
    pub(super) fn resolved_and_staged(&self, json: bool, out: &mut impl Write) -> Res<bool> {
        if self.index()?.entries.iter().any(|e| e.stage() > 0) {
            return Err("rebase --continue: resolve the conflicts and `alt add` them first".into());
        }
        if self.unstaged()?.is_empty() {
            return Ok(true);
        }
        let said =
            "You must edit all merge conflicts and then\nmark them as resolved using git add";
        if json {
            return Err(crate::machine::Failure::refusal("precondition_failed", said).into());
        }
        writeln!(out, "{said}")?;
        Ok(false)
    }

    /// `alt rebase --skip`: drop the stopped step and go on. Git resets
    /// hard here and loses what is not staged; the steps that follow may
    /// write any file, so a change that is not staged, in a file that is
    /// not in conflict, refuses the skip.
    pub fn rebase_skip(&mut self, json: bool, out: &mut impl Write) -> Res<bool> {
        let mut state = self
            .rebase_state()?
            .ok_or("rebase --skip: no rebase in progress")?;
        // the steps that follow are committed by whoever skips this one
        self.committer()?;
        let index = self.index()?;
        let conflicted = index.entries.iter().filter(|e| e.stage() > 0);
        let conflicted: HashSet<&BString> = conflicted.map(|e| &e.path).collect();
        let unstaged = self.unstaged()?;
        if let Some(path) = unstaged.iter().filter(|p| !conflicted.contains(p)).min() {
            return Err(crate::machine::Failure::refusal(
                "precondition_failed",
                format!(
                    "rebase --skip: '{path}' has changes that are not staged, and the steps \
                     that follow may write over them; stage them or take them back first"
                ),
            )
            .into());
        }
        let head = self.rebase_head()?;
        (state.current, state.merging, state.ran) = (None, None, false);
        self.save_rebase_state(&state)?;
        self.reset_worktree_to(head)?;
        self.settle_state()?;
        self.run_rebase(state, json, out)
    }

    /// `alt rebase --abort`: back to the branch as it was; the branch never
    /// moved, so only HEAD, the index and the working tree change. A
    /// file that is written loses a change that is not staged, as by the
    /// hard reset of git, and an undo brings it back; one that is not
    /// written keeps it.
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
