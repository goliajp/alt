//! `alt rebase`: replay a branch's own commits onto another base, as git does,
//! optionally through an edited todo list (pick / reword / squash / fixup /
//! drop).
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
mod plan;
mod run;
mod state;
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
        let (interactive, json) = (args.interactive, args.json);
        run::ensure_commands(&args.exec)?;
        // each commit is replayed anew where a command follows it or its date changes
        let anew = !args.exec.is_empty() || args.reset_author_date;
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

        let commits = self.commits_to_replay(orig, onto)?;
        let short = branch.strip_prefix("refs/heads/").unwrap_or(&branch);
        if !interactive && !anew && self.merge_bases(&[orig], &[onto])? == [onto] {
            writeln!(out, "Current branch {short} is up to date.")?;
            return Ok(false);
        }
        let mut todo: Vec<(Action, ObjectId)> =
            commits.iter().map(|&c| (Action::Pick, c)).collect();
        if interactive {
            todo = self.edit_todo(&todo, onto, orig)?;
            if todo.is_empty() {
                writeln!(out, "Nothing to do")?;
                return Ok(false);
            }
        }

        self.ensure_rebase_may_end(orig, onto, &todo)?;
        let picked = todo.iter().filter(|step| step.0 != Action::Drop);
        let (old, target) = self.replay_trees(orig, onto, picked.map(|step| step.1))?;
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
            todo: run::steps(&todo, &args.exec),
            fresh_dates: args.reset_author_date,
            ran: false,
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
            let (action, commit) = match state.todo.remove(0) {
                Step::Replay(action, commit) => (action, commit),
                Step::Run(command) => {
                    // whoever gave this command runs that one, once what
                    // was done so far holds and nothing is held
                    state.ran = true;
                    self.save_rebase_state(&state)?;
                    self.settle_state()?;
                    crate::exec_step::ask(command, self.root.clone());
                    return Ok(false);
                }
            };
            if action == Action::Drop {
                continue;
            }
            let head = self.rebase_head()?;
            if action == Action::Pick && !state.fresh_dates && self.stands_on(commit, head)? {
                self.save_rebase_state(&state)?;
                self.step_onto(head, commit)?;
                continue;
            }
            // that it stopped at this step holds with the conflict it writes
            state.current = Some((action, commit));
            let stopped = state.text();
            state.current = None;
            match self.pick(head, commit, stopped)? {
                Picked::Conflicted(conflicts) => {
                    self.report_rebase_stop(commit, &conflicts, json, out)?;
                    return Ok(true);
                }
                Picked::Clean(tree) if tree == self.commit_tree(head)? && !action.melds() => {
                    // its changes are already in the new base: nothing left to replay
                }
                Picked::Clean(tree) => {
                    // that the step is done holds with its commit
                    self.save_rebase_state(&state)?;
                    self.apply_step(action, commit, tree, state.fresh_dates)?;
                }
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
        let moment = self.moment();
        let mut author = parse_sig(&author_line)?;
        if fresh_date && let Some((when, tz)) = moment.split_once(' ') {
            (author.when, author.tz) = (when.parse()?, tz);
        }
        let me = self.committer()?;
        let scratch = self.scratch_ref();
        self.record_commit(
            NewCommit {
                effects: recovery::RefEffects::Unknown,
                tree,
                parents: &parents,
                author: &author,
                committer: &me.sig(),
                message: &message,
            },
            &scratch,
            Some(head),
            "rebase",
            // the note follows the commit it replaces: the replayed one, or
            // for squash and fixup the one it melds into
            &MetaInput {
                carry_from: Some(if action.melds() { head } else { commit }),
                ..Default::default()
            },
        )?;
        Ok(())
    }

    fn report_rebase_stop(
        &self,
        commit: ObjectId,
        conflicts: &[BString],
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        if json {
            return self.report_merge(true, out, "conflicted", None, conflicts, "");
        }
        cherry_pick::say_conflicts(out, "Merge", conflicts)?;
        let subject = self.read_commit_parts(commit)?.subject();
        writeln!(
            out,
            "error: could not apply {} {subject}\nResolve the conflicts, `alt add` them and run \
             `alt rebase --continue` (or `--skip`, `--abort`).",
            &commit.to_string()[..7]
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

    pub(super) fn committer_time(&self, commit: ObjectId) -> Res<i64> {
        let obj = self
            .store
            .odb
            .get(&commit)?
            .ok_or("commit missing from store")?;
        Ok(alt_git_codec::Commit::parse(&obj.data)?
            .committer_date()
            .unwrap_or(0))
    }

    pub(super) fn read_commit_parts(&self, commit: ObjectId) -> Res<CommitParts> {
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

pub(super) struct CommitParts {
    pub(super) parents: Vec<ObjectId>,
    pub(super) author: String,
    pub(super) message: String,
}

impl CommitParts {
    pub(super) fn subject(&self) -> String {
        self.message.lines().next().unwrap_or("").to_owned()
    }
}
