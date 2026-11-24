//! The steps of a rebase that are shell commands, and a commit that is
//! replayed by stepping onto it.

use super::super::sequence_editor::Action;
use super::super::*;
use super::state::Step;

/// Refuses, in git's words, a command that cannot be a step.
pub(super) fn ensure_commands(commands: &[String]) -> Res<()> {
    for command in commands {
        let said = if command.trim().is_empty() {
            "error: empty exec command\n"
        } else if command.contains('\n') {
            "error: exec commands cannot contain newlines\n"
        } else {
            continue;
        };
        return Err(said::refused(said, 1));
    }
    Ok(())
}

/// The steps of a rebase: the commits of `todo`, and `commands` after
/// each commit that is made, as git lists them.
pub(super) fn steps(todo: &[(Action, ObjectId)], commands: &[String]) -> Vec<Step> {
    let run = || commands.iter().cloned().map(Step::Run);
    let mut steps = Vec::new();
    let mut made = false;
    for &(action, commit) in todo {
        // a commit that melds into the one before is part of it
        if made && !action.melds() && action != Action::Drop {
            steps.extend(run());
        }
        made |= action != Action::Drop;
        steps.push(Step::Replay(action, commit));
    }
    if made {
        steps.extend(run());
    }
    steps
}

impl NativeRepo<'_> {
    /// Whether `commit` is the child of `head` alone, so that replaying
    /// it there makes it again.
    pub(super) fn stands_on(&self, commit: ObjectId, head: ObjectId) -> Res<bool> {
        Ok(self.commit_parents(commit)? == [head])
    }

    /// Replays `commit` onto its own parent `head`: the rebase steps
    /// onto the commit itself, as git fast-forwards to it.
    pub(super) fn step_onto(&mut self, head: ObjectId, commit: ObjectId) -> Res<()> {
        let onto = RefChange {
            name: self.scratch_ref(),
            old: Some(RefTarget::Oid(head)),
            new: Some(RefTarget::Oid(commit)),
        };
        let old = index_entries(&self.index()?);
        let target = self.commit_entries(commit)?;
        self.commit_refs_checkout("rebase", &[onto], &old, &target)?;
        Ok(())
    }

    /// Before a rebase goes on from a shell command it stopped for:
    /// refuses to where the index or the files hold changes, in git's
    /// words where the command was just run and left them.
    pub(super) fn ensure_left_clean(&self) -> Res<()> {
        let Err(dirty) = self.ensure_clean("rebase --continue") else {
            return Ok(());
        };
        let Some(command) = crate::exec_step::just_ran() else {
            return Err(dirty);
        };
        let said = format!(
            "error: execution succeeded: {command}\nbut left changes to your index and/or \
             working tree.\nCommit or stash your changes, and then run\n\n  \
             alt rebase --continue\n\n"
        );
        Err(said::refused(&said, 1))
    }
}
