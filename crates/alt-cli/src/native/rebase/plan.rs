//! What a rebase is to replay, and the editor's part in it: the list of
//! steps and the messages a step asks for.

use super::super::sequence_editor::{self as ed, Action};
use super::super::*;
use super::state::Step;

/// The commits of a branch's own, each with its parents.
pub(super) type Own = std::collections::HashMap<ObjectId, Vec<ObjectId>>;

impl NativeRepo<'_> {
    /// The commits reachable from `orig` but not from `onto`, each with
    /// its parents, and their order, oldest first: the reverse of git's
    /// `--topo-order`, in which a commit comes before its parents and
    /// what a merge brought in before the line it was merged into.
    pub(super) fn own_commits(&self, orig: ObjectId, onto: ObjectId) -> Res<(Vec<ObjectId>, Own)> {
        let from_orig = self.ancestry(&[orig])?;
        let from_onto = self.ancestry(&[onto])?;
        let mine: Own = from_orig
            .into_iter()
            .filter(|(c, _)| !from_onto.contains_key(c))
            .collect();
        let own = |c: &&ObjectId| mine.contains_key(*c);
        let mut children: std::collections::HashMap<ObjectId, usize> = Default::default();
        for parent in mine.values().flatten().filter(own) {
            *children.entry(*parent).or_default() += 1;
        }
        // the commits to walk from, the newest on top
        let mut next = Vec::new();
        for commit in mine.keys().filter(|c| !children.contains_key(*c)) {
            next.push((self.committer_time(*commit)?, *commit));
        }
        next.sort();
        let mut next: Vec<ObjectId> = next.into_iter().map(|(_, commit)| commit).collect();
        let mut order = Vec::new();
        while let Some(commit) = next.pop() {
            order.push(commit);
            for parent in mine[&commit].iter().filter(own) {
                let left = children.get_mut(parent).expect("counted above");
                *left -= 1;
                if *left == 0 {
                    next.push(*parent);
                }
            }
        }
        order.reverse();
        Ok((order, mine))
    }

    /// The steps of the rebase `args` asks for, of `branch` at `orig` onto
    /// `onto`, as the editor left them where it asks for one; `None`
    /// where there is nothing to do, which is said.
    pub(super) fn planned(
        &mut self,
        args: &crate::commit_args::Rebase,
        branch: &str,
        orig: ObjectId,
        onto: ObjectId,
        out: &mut impl Write,
    ) -> Res<Option<Vec<Step>>> {
        // each commit is replayed anew where a command follows it, its
        // date changes or merges are kept
        let anew = !args.exec.is_empty() || args.reset_author_date || args.rebase_merges;
        let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
        if !args.interactive && !anew && self.merge_bases(&[orig], &[onto])? == [onto] {
            writeln!(out, "Current branch {short} is up to date.")?;
            return Ok(None);
        }
        let todo = match args.rebase_merges {
            true => self.steps_with_merges(orig, onto)?,
            false => {
                let commits = self.commits_to_replay(orig, onto)?;
                let picks = commits.into_iter().map(|c| Step::Replay(Action::Pick, c));
                picks.collect()
            }
        };
        let mut todo = super::run::steps(todo, &args.exec);
        if args.interactive {
            todo = self.edit_todo(&todo, onto, orig)?;
            if todo.is_empty() {
                writeln!(out, "Nothing to do")?;
                return Ok(None);
            }
            let commands = todo.iter().filter_map(|step| match step {
                Step::Run(command) => Some(command.clone()),
                _ => None,
            });
            super::run::ensure_commands(&commands.collect::<Vec<_>>())?;
        }
        Ok(Some(todo))
    }

    /// Lists the commits a rebase that keeps no merges will replay, oldest
    /// first: its own commits without the merges (git drops them too).
    pub(super) fn commits_to_replay(&self, orig: ObjectId, onto: ObjectId) -> Res<Vec<ObjectId>> {
        let (order, mine) = self.own_commits(orig, onto)?;
        Ok(order.into_iter().filter(|c| mine[c].len() < 2).collect())
    }

    pub(super) fn edit_todo(
        &mut self,
        todo: &[Step],
        onto: ObjectId,
        orig: ObjectId,
    ) -> Res<Vec<Step>> {
        let mut text = String::new();
        for step in todo {
            let short = |c: &ObjectId| c.to_string()[..7].to_owned();
            text.push_str(&match step {
                Step::Replay(action, c) => {
                    let subject = self.read_commit_parts(*c)?.subject();
                    format!("{} {} {subject}\n", action.word(), short(c))
                }
                Step::Run(command) => format!("exec {command}\n"),
                Step::Label(name) => format!("label {name}\n"),
                Step::Reset(to) => format!("reset {to}\n"),
                Step::Merge(c, brought) => {
                    let subject = self.read_commit_parts(*c)?.subject();
                    format!("merge -C {} {} # {subject}\n", short(c), brought.join(" "))
                }
            });
        }
        let (o, h) = (&onto.to_string()[..7], &orig.to_string()[..7]);
        text.push_str(&format!(
            "\n# Rebase {o}..{h} onto {o} ({} command{})\n",
            todo.len(),
            if todo.len() == 1 { "" } else { "s" }
        ));
        text.push_str(ed::TODO_HELP);
        ed::check_machine_mode()?;
        let path = self.index_path.with_file_name("REBASE_TODO");
        crate::durable::write(&path, text)?;
        ed::edit(&path, true)?;
        let edited = std::fs::read_to_string(&path)?;
        crate::durable::remove_file(&path)?;
        let repo = self.store.git_view()?;
        ed::parse_todo(&edited, |rev| {
            repo.rev_parse(rev)?
                .ok_or_else(|| format!("rebase: unknown commit '{rev}' in the todo list").into())
        })
    }

    pub(super) fn squash_message(&self, first: &str, next: &str) -> Res<String> {
        let text = format!(
            "# This is a combination of 2 commits.\n# This is the 1st commit message:\n\n{first}\n\
             # This is the commit message #2:\n\n{next}"
        );
        self.edited_message(&text)
    }

    /// Runs the editor on a commit message and applies git's cleanup.
    pub(super) fn edited_message(&self, message: &str) -> Res<String> {
        ed::check_machine_mode()?;
        let path = self.index_path.with_file_name("REBASE_MSG");
        crate::durable::write(&path, message)?;
        ed::edit(&path, false)?;
        let edited = ed::cleanup(&std::fs::read_to_string(&path)?);
        crate::durable::remove_file(&path)?;
        if edited.is_empty() {
            return Err("rebase: aborting because the commit message is empty".into());
        }
        Ok(edited)
    }

    pub(in crate::native) fn committer_time(&self, commit: ObjectId) -> Res<i64> {
        let obj = self
            .store
            .odb
            .get(&commit)?
            .ok_or("commit missing from store")?;
        Ok(alt_git_codec::Commit::parse(&obj.data)?
            .committer_date()
            .unwrap_or(0))
    }

    pub(in crate::native) fn read_commit_parts(&self, commit: ObjectId) -> Res<CommitParts> {
        let obj = self
            .store
            .odb
            .get(&commit)?
            .ok_or("commit missing from store")?;
        let c = alt_git_codec::Commit::parse(&obj.data)?;
        Ok(CommitParts {
            parents: c.parents().collect(),
            author: c
                .author()
                .ok_or("commit has no author")?
                .to_str()
                .map_err(|_| "rebase: an author is not UTF-8")?
                .to_owned(),
            message: c
                .message()
                .to_str()
                .map_err(|_| "rebase: a commit message is not UTF-8")?
                .to_owned(),
        })
    }
}

pub(in crate::native) struct CommitParts {
    pub(in crate::native) parents: Vec<ObjectId>,
    pub(in crate::native) author: String,
    pub(in crate::native) message: String,
}

impl CommitParts {
    pub(in crate::native) fn subject(&self) -> String {
        self.message.lines().next().unwrap_or("").to_owned()
    }
}
