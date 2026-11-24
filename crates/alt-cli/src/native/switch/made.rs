//! The branch a switch makes, or puts elsewhere, before it goes to it.

use super::*;

impl NativeRepo<'_> {
    /// Creates the branch `full` at the commit `start` names and moves
    /// HEAD to it as a switch to a branch there would, in one operation;
    /// what is left that differs from that commit.
    pub(super) fn switch_create_at(
        &mut self,
        name: &str,
        full: &str,
        current: &str,
        start: &str,
    ) -> Res<Vec<(char, BString)>> {
        check_branch_name(name)?;
        self.ensure_no_branch(name)?;
        let commit = self
            .commit_named(start)?
            .ok_or_else(|| settings::fatal(format!("invalid reference: {start}")))?;
        self.switch_to(full, current, Some(commit))
    }

    /// Creates the branch `full` at the current commit and moves HEAD to
    /// it. The working tree and the index carry over unchanged, so no
    /// checkout is needed.
    pub(super) fn switch_create(&mut self, name: &str, full: &str, current: &str) -> Res<()> {
        check_branch_name(name)?;
        self.ensure_no_branch(name)?;
        if let Some(commit) = self.store.refs.resolve(current)? {
            // non-terminal tx of switch-with-create: the move_head below
            // is the one that carries this command's idempotency key.
            self.commit_refs(
                "branch",
                &[RefChange {
                    name: full.to_owned(),
                    old: None,
                    new: Some(RefTarget::Oid(commit)),
                }],
            )?;
        }
        self.move_head(current, full)
    }

    /// `switch -C`: puts the branch `full`, which there is, at the commit
    /// `start` names, the current one where nothing is named, and moves
    /// HEAD to it as a switch would; what is left that differs from that
    /// commit.
    pub(super) fn switch_reset(
        &mut self,
        name: &str,
        full: &str,
        current: &str,
        start: Option<&str>,
    ) -> Res<Vec<(char, BString)>> {
        if full != current
            && let Some(at) = self.held_at(full, false)?
        {
            let said = format!("'{name}' is already used by worktree at '{}'", at.display());
            return Err(settings::fatal(said));
        }
        let commit = match start {
            Some(start) => self.commit_named(start)?,
            None => self.store.refs.resolve(current)?,
        };
        let nothing = || settings::fatal(format!("invalid reference: {}", start.unwrap_or("HEAD")));
        let commit = commit.ok_or_else(nothing)?;
        if self.store.refs.resolve(full)? != Some(commit) {
            self.ensure_may_move(name)?;
        }
        self.switch_to(full, current, Some(commit))
    }
}
