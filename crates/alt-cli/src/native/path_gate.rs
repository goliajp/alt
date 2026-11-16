//! The path gate: what a principal under a policy with `path_allow` may
//! stage and commit.

use std::collections::{HashMap, HashSet};

use super::*;

impl NativeRepo<'_> {
    /// Reject staging/committing a path that the policy's `path_allow` does
    /// not match. Empty allow-list = no constraint.
    pub(super) fn ensure_path_allowed(&self, path: &[u8]) -> Res<()> {
        // a pattern is text: where there are patterns, so is every path allowed
        let text = std::str::from_utf8(path).ok();
        if !self.caps.path_allow.is_empty() && !text.is_some_and(|p| self.caps.allows_path_name(p))
        {
            let path = String::from_utf8_lossy(path);
            let why = format!("capability denied: path '{path}' is not in path_allow");
            return Err(crate::machine::Failure::refusal("capability_denied", why).into());
        }
        Ok(())
    }

    /// Refuses a commit of `tree` in the place of `old` when a path it
    /// adds, changes or removes is one the policy leaves out, whoever
    /// staged it: what another principal staged is not committed in this
    /// one's name, by `commit`, by an amend or by the step a stopped rebase
    /// or revert goes on with. What `tree` has as `old` has it is not
    /// looked at.
    pub(super) fn ensure_commit_allowed(&self, old: Option<ObjectId>, tree: ObjectId) -> Res<()> {
        if self.caps.path_allow.is_empty() {
            return Ok(());
        }
        let was = match old {
            Some(old) => self.commit_entries(old)?,
            None => Vec::new(),
        };
        let now = flatten_tree(&self.store.odb, tree, self.store.algo)?;
        self.ensure_difference_allowed(&was, &now)
    }

    /// The trees of `orig` and `onto`, for a replay of `commits` that
    /// begins with a checkout from the one to the other. Refused before any
    /// of it is done when that checkout would be, or when a path one of the
    /// commits changes is one the policy leaves out: a replay refused
    /// midway would leave the branch between where it was and where it goes.
    pub(super) fn replay_trees(
        &self,
        orig: ObjectId,
        onto: ObjectId,
        commits: impl Iterator<Item = ObjectId>,
    ) -> Res<(Vec<WorkEntry>, Vec<WorkEntry>)> {
        let (old, target) = (self.commit_entries(orig)?, self.commit_entries(onto)?);
        self.preflight_checkout(&old, &target)?;
        if self.caps.path_allow.is_empty() {
            return Ok((old, target));
        }
        for commit in commits {
            let was = match self.commit_parents(commit)?.first() {
                Some(&parent) => self.commit_entries(parent)?,
                None => Vec::new(),
            };
            self.ensure_difference_allowed(&was, &self.commit_entries(commit)?)?;
        }
        Ok((old, target))
    }

    /// Every path `now` adds to `was`, changes or leaves out is allowed.
    fn ensure_difference_allowed(&self, was: &[WorkEntry], now: &[WorkEntry]) -> Res<()> {
        let was: HashMap<&BString, &WorkEntry> = was.iter().map(|e| (&e.path, e)).collect();
        let same = |e: &WorkEntry, old: &&WorkEntry| old.oid == e.oid && old.mode == e.mode;
        for e in now
            .iter()
            .filter(|e| !was.get(&e.path).is_some_and(|old| same(e, old)))
        {
            self.ensure_path_allowed(&e.path)?;
        }
        let kept: HashSet<&BString> = now.iter().map(|e| &e.path).collect();
        for gone in was.keys().filter(|path| !kept.contains(*path)) {
            self.ensure_path_allowed(gone)?;
        }
        Ok(())
    }
}
