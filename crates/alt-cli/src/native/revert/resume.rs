//! A revert that stopped, gone on with or given up.

use std::collections::{HashMap, HashSet};

use super::super::merge::Resolved;
use super::super::*;
use super::RevertState;

/// Refuses, in git's words, the step of `verb` whose merge `resolved`
/// writes one of the files `dirty`, which are changed and not staged;
/// `ours` is what the index has.
pub(super) fn ensure_none_overwritten(
    ours: &[WorkEntry],
    resolved: &[Resolved],
    dirty: &HashSet<BString>,
    verb: &str,
) -> Res<()> {
    if dirty.is_empty() {
        return Ok(());
    }
    let had: HashMap<&BString, (ObjectId, u32)> =
        ours.iter().map(|e| (&e.path, (e.oid, e.mode))).collect();
    let written = |r: &&Resolved| {
        r.conflicted || r.entry.as_ref().map(|e| (e.oid, e.mode)) != had.get(&r.path).copied()
    };
    let lost = resolved.iter().filter(written).map(|r| &r.path);
    let mut lost: Vec<&BString> = lost.filter(|path| dirty.contains(*path)).collect();
    if lost.is_empty() {
        return Ok(());
    }
    lost.sort();
    let mut said = "error: Your local changes to the following files would be overwritten by \
                    merge:\n"
        .to_owned();
    for path in lost {
        said.push_str(&format!("\t{path}\n"));
    }
    said.push_str("Please commit your changes or stash them before you merge.\nAborting\n");
    said.push_str(&format!("fatal: {verb} failed\n"));
    Err(said::refused(&said, 128))
}

impl NativeRepo<'_> {
    /// The tracked files that are changed or gone and not staged so.
    pub(in crate::native) fn unstaged(&self) -> Res<HashSet<BString>> {
        let state = self.tree_state(alt_worktree::Untracked::No)?;
        // what a submodule has checked out is nothing a checkout writes over
        let changed = state.delta.changed.iter().filter(|w| w.mode != 0o160000);
        let changed = changed.map(|w| w.path.clone());
        Ok(changed.chain(state.delta.deleted.iter().cloned()).collect())
    }

    /// Whether `branch` came to `head` by a revert: the sequence made
    /// the commit, and no one moved the branch since.
    fn left_by_revert(&self, branch: &str, head: ObjectId) -> Res<bool> {
        let log = self.store.git_view()?.reflog(branch)?;
        let by_revert = |said: &Vec<u8>| said.starts_with(b"revert");
        Ok(log.last().is_some_and(|last| {
            last.new == Some(head) && last.said.as_ref().is_some_and(by_revert)
        }))
    }

    /// `alt revert --continue`: commit the resolved revert, then go on with
    /// the rest of the sequence.
    pub fn revert_continue(&mut self, json: bool, out: &mut impl Write) -> Res<bool> {
        let state = self
            .revert_state()?
            .ok_or("revert --continue: no revert in progress")?;
        self.ensure_signing()?;
        if let Some(current) = state.current {
            if self.index()?.entries.iter().any(|e| e.stage() > 0) {
                return Err(
                    "revert --continue: resolve the conflicts and `alt add` them first".into(),
                );
            }
            let head = self.branch_tip("revert --continue")?;
            let head_tree = self.commit_tree(head)?;
            let tree = self.staged_tree(Some(head_tree))?;
            let msg = self.revert_message(current, state.mainline)?;
            // the rest of the sequence waits from this commit on
            let rest = RevertState {
                current: None,
                todo: state.todo.clone(),
                ..state
            };
            self.save_revert_rest(&rest)?;
            let commit = self.commit_revert(head, tree, &msg)?;
            self.report_revert(commit, &msg, json, out)?;
        }
        let dirty = self.unstaged()?;
        self.run_reverts(state.todo, state.mainline, state.orig, &dirty, json, out)
    }

    /// `alt revert --abort`: put the branch, index and working tree back to
    /// where they were before the sequence started. A branch that was
    /// moved since the sequence stopped is left where it is.
    pub fn revert_abort(&mut self, out: &mut impl Write) -> Res<()> {
        let state = self
            .revert_state()?
            .ok_or("revert --abort: no revert in progress")?;
        let branch = self.head_branch()?;
        let head = self.branch_tip("revert --abort")?;
        if head != state.orig && !self.left_by_revert(&branch, head)? {
            // as git: the branch is the user's by now, and so is the tree
            self.clear_revert_state()?;
            self.settle_state()?;
            let moved = "warning: You seem to have moved HEAD. Not rewinding, check your HEAD!\n";
            return Ok(crate::diagnostics::emit(moved.as_bytes())?);
        }
        // the branch moves back with the reset, in one ref transaction
        let back = RefChange {
            name: branch,
            old: Some(RefTarget::Oid(head)),
            new: Some(RefTarget::Oid(state.orig)),
        };
        let moved = if head != state.orig {
            vec![back]
        } else {
            Vec::new()
        };
        self.clear_revert_state()?;
        self.reset_merge(state.orig, "revert --abort", &moved)?;
        self.settle_state()?;
        writeln!(out, "Revert aborted.")?;
        Ok(())
    }
}
