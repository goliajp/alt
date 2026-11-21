//! `alt prompt` in a repository.

use alt_worktree::Untracked;

use super::in_progress::OperationKind;
use super::*;
use crate::prompt::{PromptArgs, State};

/// The most commits looked at to tell how far a branch is from a remote's.
const MOST_COMMITS: usize = 10_000;

impl NativeRepo<'_> {
    /// `alt prompt`: where HEAD is, what is under way, how much is
    /// changed, and how far the branch is from each remote's. What cannot
    /// be read is left out and told as `attention`.
    pub fn prompt(&self, args: &PromptArgs, out: &mut impl Write) -> Res<()> {
        let mut state = State::default();
        let mut failed: Vec<Box<dyn std::error::Error>> = Vec::new();
        state.say("repo", self.root.display().to_string());
        state.say("workspace", self.workspace.clone());

        let untracked = match args.no_untracked {
            true => Ok(Untracked::No),
            false => self.untracked_mode(None),
        };
        let trees = untracked.and_then(|mode| self.tree_state(mode));
        let operation = self.operation().unwrap_or_else(|error| {
            failed.push(error);
            None
        });

        let head = self.store.refs.resolve(&self.head_ref)?;
        let branch = self.prompt_branch(operation.as_ref())?;
        match (&branch, head) {
            (Some(branch), Some(_)) => state.say("branch", branch.clone()),
            (Some(branch), None) => state.say("unborn", branch.clone()),
            (None, Some(head)) => state.say("detached", head.to_string()[..8].to_owned()),
            (None, None) => {}
        }
        // a tag object has to be read to know its commit, so tags are only
        // looked through where there is no branch to show
        if let (None, Some(head)) = (&branch, head) {
            match self.tag_at(head) {
                Ok(Some(tag)) => state.say("tag", tag),
                Ok(None) => {}
                Err(error) => failed.push(error),
            }
        }
        if let Some(operation) = &operation {
            state.say("operation", operation.kind.name());
            if let Some((step, steps)) = operation.step {
                state.say("step", format!("{step} {steps}"));
            }
        }
        match trees {
            Ok(trees) => {
                let unmerged = trees.conflicts();
                let status = trees.status();
                let free = |path: &BString| !unmerged.contains_key(path);
                let counts = [
                    (
                        "staged",
                        status.staged.iter().filter(|(p, _)| free(p)).count(),
                    ),
                    (
                        "unstaged",
                        status.unstaged.iter().filter(|(p, _)| free(p)).count(),
                    ),
                    (
                        "untracked",
                        status.untracked.iter().filter(|p| free(p)).count(),
                    ),
                    ("unmerged", unmerged.len()),
                ];
                for (key, count) in counts.into_iter().filter(|(_, count)| *count > 0) {
                    state.say(key, count.to_string());
                }
            }
            Err(error) => failed.push(error),
        }
        if let (false, Some(branch), Some(head)) = (args.no_ahead_behind, &branch, head)
            && let Err(error) = self.prompt_remotes(branch, head, &mut state)
        {
            failed.push(error);
        }
        let pending = failed.iter().any(|error| {
            let failure = error.downcast_ref::<crate::machine::Failure>();
            failure.is_some_and(|failure| failure.operation_id.is_some())
        });
        if pending {
            state.say("attention", "recovery");
        } else if !failed.is_empty() {
            state.say("attention", "damaged");
        } else if alt_refs::left_out().is_some() {
            state.say("attention", "unconfirmed");
        }
        Ok(out.write_all(state.render(&args.format).as_bytes())?)
    }

    /// The branch HEAD is on, without `refs/heads/`: the branch being
    /// rebased while a rebase has HEAD, none while a bisect has it or
    /// HEAD names a commit.
    fn prompt_branch(&self, operation: Option<&in_progress::Operation>) -> Res<Option<String>> {
        let short = |name: &str| name.strip_prefix("refs/heads/").unwrap_or(name).to_owned();
        Ok(match (operation, self.store.refs.get(&self.head_ref)) {
            (Some(operation), _) if operation.kind == OperationKind::Bisect => None,
            (Some(operation), _) if operation.branch.is_some() => {
                operation.branch.as_deref().map(short)
            }
            (_, Some(RefTarget::Symbolic(name))) => Some(short(name)),
            (_, Some(RefTarget::Oid(_))) => None,
            (_, None) => Some("main".to_owned()),
        })
    }

    /// The first tag in name order that names `commit`, itself or through
    /// a tag object.
    fn tag_at(&self, commit: ObjectId) -> Res<Option<String>> {
        let tags = self.store.refs.iter();
        let tags =
            tags.filter_map(|(name, target)| Some((name.strip_prefix("refs/tags/")?, target)));
        let mut tags: Vec<(&str, &RefTarget)> = tags.collect();
        tags.sort_by_key(|(name, _)| *name);
        for (name, target) in tags {
            let RefTarget::Oid(mut oid) = *target else {
                continue;
            };
            while oid != commit {
                let Some(object) = self.store.odb.get(&oid)? else {
                    break;
                };
                if object.kind != ObjectKind::Tag {
                    break;
                }
                let Some(tagged) = alt_git_codec::Tag::parse(&object.data)?.object() else {
                    break;
                };
                oid = tagged;
            }
            if oid == commit {
                return Ok(Some(name.to_owned()));
            }
        }
        Ok(None)
    }

    /// For each remote that has a branch of the name of ours, how far
    /// ours is ahead of it and behind it.
    fn prompt_remotes(&self, branch: &str, head: ObjectId, state: &mut State) -> Res<()> {
        for remote in self.read_remotes()? {
            let theirs = format!("refs/remotes/{}/{branch}", remote.name);
            let Some(theirs) = self.store.refs.resolve(&theirs)? else {
                continue;
            };
            let distance = self.ahead_behind(head, theirs, MOST_COMMITS)?;
            let more = if distance.cut { "+" } else { "" };
            let (ahead, behind) = (distance.ahead, distance.behind);
            state.say(
                "remote",
                format!("{} {ahead}{more} {behind}{more}", remote.name),
            );
        }
        Ok(())
    }
}
