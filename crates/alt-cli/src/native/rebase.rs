//! `alt rebase`: replay a branch's own commits onto another base, as git does,
//! optionally through an edited todo list (pick / reword / squash / fixup /
//! drop), with its merges made again where `-r` asks.
//!
//! While it runs, the workspace HEAD points at a scratch ref that carries the
//! commits made so far; the branch itself only moves when the rebase finishes,
//! in one ref transaction with HEAD going back to it, so `--abort` has
//! nothing to rewind.

use super::commit::{NewCommit, parse_sig};
use super::notes::MetaInput;
use super::sequence_editor::Action;
use super::*;

mod leave;
mod merges;
mod plan;
mod run;
mod state;
mod steps;
pub(super) use state::RebaseState;
use state::Step;

enum Picked {
    Clean(ObjectId),
    Conflicted(Vec<BString>),
}

impl NativeRepo<'_> {
    /// `alt rebase [-i] [-x <command>] <upstream>`. Returns whether it
    /// stopped on a conflict.
    pub fn rebase(&mut self, args: &crate::commit_args::Rebase, out: &mut impl Write) -> Res<bool> {
        let upstream = args.upstream.as_deref().unwrap_or_default();
        let json = args.json;
        run::ensure_commands(&args.exec)?;
        self.ensure_topic_branch_or_unborn("rebase")?;
        self.ensure_no_merge_in_progress("rebase")?;
        self.ensure_clean("rebase")?;
        self.committer()?;
        let branch = self.head_branch()?;
        if !branch.starts_with("refs/heads/") {
            return Err("rebase: HEAD is not on a branch".into());
        }
        let repo = self.store.git_view()?;
        let onto = repo
            .rev_parse(upstream)?
            .ok_or_else(|| format!("rebase: bad revision '{upstream}'"))?;
        let orig = self
            .store
            .refs
            .resolve(&branch)?
            .ok_or("rebase: the branch has no commits yet")?;

        let Some(todo) = self.planned(args, &branch, orig, onto, out)? else {
            return Ok(false);
        };

        self.ensure_rebase_may_end(orig, onto, &todo)?;
        let made = todo.iter().filter_map(Step::made);
        let (old, target) = self.replay_trees(orig, onto, made)?;
        let recovery = Some(self.snapshot_workspace()?);

        // hand HEAD to the scratch ref, sitting on the new base: the state
        // file, the refs and the files of the base change as one
        let scratch = self.scratch_ref();
        let moved = [
            RefChange {
                name: scratch.clone(),
                old: None,
                new: Some(RefTarget::Oid(onto)),
            },
            RefChange {
                name: self.head_ref.clone(),
                old: Some(RefTarget::Symbolic(branch.clone())),
                new: Some(RefTarget::Symbolic(scratch)),
            },
        ];
        let state = RebaseState {
            recovery,
            branch,
            orig,
            onto,
            current: None,
            todo,
            fresh_dates: args.reset_author_date,
            ran: false,
            labels: Vec::new(),
            merging: None,
        };
        self.save_rebase_state(&state)?;
        self.commit_refs_checkout("rebase", &moved, &old, &target)?;
        self.run_rebase(state, json, out)
    }

    /// `alt rebase --continue`: commit the resolved step, then go on.
    pub fn rebase_continue(&mut self, json: bool, out: &mut impl Write) -> Res<bool> {
        let mut state = self
            .rebase_state()?
            .ok_or("rebase --continue: no rebase in progress")?;
        self.committer()?;
        if let Some((action, commit)) = state.current {
            if self.index()?.entries.iter().any(|e| e.stage() > 0) {
                return Err(
                    "rebase --continue: resolve the conflicts and `alt add` them first".into(),
                );
            }
            let head_tree = self.commit_tree(self.rebase_head()?)?;
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
            if self.index()?.entries.iter().any(|e| e.stage() > 0) {
                return Err(
                    "rebase --continue: resolve the conflicts and `alt add` them first".into(),
                );
            }
            let head_tree = self.commit_tree(self.rebase_head()?)?;
            let tree = self.staged_tree(Some(head_tree))?;
            state.merging = None;
            self.save_rebase_state(&state)?;
            self.record_merge_again(commit, theirs, tree, state.fresh_dates)?;
        } else if state.ran {
            self.ensure_left_clean()?;
        }
        state.ran = false;
        self.run_rebase(state, json, out)
    }

    fn run_rebase(
        &mut self,
        mut state: RebaseState,
        json: bool,
        out: &mut impl Write,
    ) -> Res<bool> {
        while !state.todo.is_empty() {
            let step = state.todo.remove(0);
            if let Some(stopped) = self.run_step(&mut state, step, json, out)? {
                return Ok(stopped);
            }
        }
        // HEAD goes back to the branch and the branch to the commits made,
        // in one ref transaction that the state ends with
        let new = self.rebase_head()?;
        let moved = (new != state.orig).then_some((state.orig, new));
        let moved = self.scratch_left(&state.branch, moved)?;
        self.clear_rebase_state()?;
        self.commit_refs("rebase", &moved)?;
        if json {
            use crate::json::Json;
            let branch = &state.branch;
            let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
            crate::json::emit(
                out,
                vec![
                    ("branch", Json::str(short)),
                    ("commit", Json::str(new.to_string())),
                    ("onto", Json::str(state.onto.to_string())),
                ],
            )?;
        } else {
            writeln!(out, "Successfully rebased and updated {}.", state.branch)?;
        }
        Ok(false)
    }

    /// Three-way merges `commit`'s own change onto `head` and, when clean,
    /// checks the result out. Where it stops, the state file becomes
    /// `stopped` with the conflict.
    fn pick(&mut self, head: ObjectId, commit: ObjectId, stopped: String) -> Res<Picked> {
        let base = match self.commit_parents(commit)?.first() {
            Some(&p) => self.commit_entries(p)?,
            None => Vec::new(),
        };
        let ours = self.commit_entries(head)?;
        let theirs = self.commit_entries(commit)?;
        let label = commit.to_string()[..7].to_owned();
        let resolved = self.merge_trees(&base, &ours, &theirs, &label)?;
        self.store.odb.flush()?;
        if resolved.iter().any(|r| r.conflicted) {
            self.plan_state("REBASE_STATE", Some(stopped))?;
            self.write_conflicted(&resolved)?;
            return Ok(Picked::Conflicted(
                resolved
                    .iter()
                    .filter(|r| r.conflicted)
                    .map(|r| r.path.clone())
                    .collect(),
            ));
        }
        let entries: Vec<WorkEntry> = resolved.into_iter().filter_map(|r| r.entry).collect();
        let tree = self.store_tree(&entries)?;
        let old = index_entries(&self.index()?);
        self.checkout(&old, &entries)?;
        Ok(Picked::Clean(tree))
    }

    /// Records one replayed step whose tree is ready: a new commit for pick
    /// and reword, a replacement of the previous one for squash and fixup.
    fn apply_step(
        &mut self,
        action: Action,
        commit: ObjectId,
        tree: ObjectId,
        fresh_date: bool,
    ) -> Res<()> {
        let head = self.rebase_head()?;
        let theirs = self.read_commit_parts(commit)?;
        let (parents, author_line, message) = if action.melds() {
            let prev = self.read_commit_parts(head)?;
            let message = match action {
                Action::Squash => self.squash_message(&prev.message, &theirs.message)?,
                _ => prev.message.clone(),
            };
            (prev.parents, prev.author, message)
        } else {
            let message = match action {
                Action::Reword => self.edited_message(&theirs.message)?,
                _ => theirs.message.clone(),
            };
            (vec![head], theirs.author, message)
        };
        let from = if action.melds() { head } else { commit };
        self.record_replayed(tree, &parents, &author_line, &message, from, fresh_date)
    }

    /// Records a commit the rebase makes on the scratch ref: by the
    /// author of `author_line`, as of now with `fresh_date`, committed by
    /// whoever rebases. The note of `from`, the commit it stands for,
    /// goes with it.
    fn record_replayed(
        &mut self,
        tree: ObjectId,
        parents: &[ObjectId],
        author_line: &str,
        message: &str,
        from: ObjectId,
        fresh_date: bool,
    ) -> Res<()> {
        let head = self.rebase_head()?;
        let moment = self.moment();
        let mut author = parse_sig(author_line)?;
        if fresh_date && let Some((when, tz)) = moment.split_once(' ') {
            (author.when, author.tz) = (when.parse()?, tz);
        }
        let me = self.committer()?;
        let scratch = self.scratch_ref();
        self.record_commit(
            NewCommit {
                effects: recovery::RefEffects::Unknown,
                tree,
                parents,
                author: &author,
                committer: &me.sig(),
                message,
            },
            &scratch,
            Some(head),
            "rebase",
            &MetaInput {
                carry_from: Some(from),
                ..Default::default()
            },
        )?;
        Ok(())
    }

    /// Says that the rebase stopped on `conflicts` at the step `what`.
    fn report_rebase_stop(
        &self,
        what: &str,
        conflicts: &[BString],
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        if json {
            return self.report_merge(true, out, "conflicted", None, conflicts, "");
        }
        cherry_pick::say_conflicts(out, "Merge", conflicts)?;
        writeln!(
            out,
            "error: could not apply {what}\nResolve the conflicts, `alt add` them and run \
             `alt rebase --continue` (or `--skip`, `--abort`)."
        )?;
        Ok(())
    }

    /// The ref carrying the replayed commits while this workspace rebases.
    pub(super) fn scratch_ref(&self) -> String {
        format!("rebase/{}", self.workspace)
    }

    pub(super) fn rebase_head(&self) -> Res<ObjectId> {
        Ok(self
            .store
            .refs
            .resolve(&self.scratch_ref())?
            .ok_or("rebase: the scratch ref is missing")?)
    }

    /// The ref changes that point HEAD back at `branch` and drop the
    /// scratch ref, and move the branch where it is `moved`.
    fn scratch_left(
        &self,
        branch: &str,
        moved: Option<(ObjectId, ObjectId)>,
    ) -> Res<Vec<RefChange>> {
        let scratch = self.scratch_ref();
        let at = self.store.refs.resolve(&scratch)?;
        let mut changes = vec![
            RefChange {
                name: self.head_ref.clone(),
                old: Some(RefTarget::Symbolic(scratch.clone())),
                new: Some(RefTarget::Symbolic(branch.to_owned())),
            },
            RefChange {
                name: scratch,
                old: at.map(RefTarget::Oid),
                new: None,
            },
        ];
        changes.extend(moved.map(|(old, new)| RefChange {
            name: branch.to_owned(),
            old: Some(RefTarget::Oid(old)),
            new: Some(RefTarget::Oid(new)),
        }));
        Ok(changes)
    }
}
