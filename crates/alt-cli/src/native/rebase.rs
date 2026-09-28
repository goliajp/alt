//! `alt rebase`: replay a branch's own commits onto another base, as git does,
//! optionally through an edited todo list (pick / reword / squash / fixup /
//! drop).
//!
//! While it runs, the workspace HEAD points at a scratch ref that carries the
//! commits made so far; the branch itself only moves when the rebase finishes,
//! in one ref transaction, so `alt undo` takes the whole rebase back and
//! `--abort` has nothing to rewind.

use super::commit::{NewCommit, parse_sig};
use super::notes::MetaInput;
use super::sequence_editor::{self as ed, Action};
use super::*;

/// What a stopped or running rebase remembers.
struct RebaseState {
    /// The branch being rebased (`refs/heads/…`).
    branch: String,
    /// Where the branch pointed before the rebase.
    orig: ObjectId,
    /// The base the commits go onto.
    onto: ObjectId,
    /// The step that stopped on a conflict, not yet committed.
    current: Option<(Action, ObjectId)>,
    /// Steps still to run, in order.
    todo: Vec<(Action, ObjectId)>,
}

enum Picked {
    Clean(ObjectId),
    Conflicted(Vec<BString>),
}

impl NativeRepo<'_> {
    /// `alt rebase [-i] <upstream>`. Returns whether it stopped on a conflict.
    pub fn rebase(
        &mut self,
        upstream: &str,
        interactive: bool,
        json: bool,
        out: &mut impl Write,
    ) -> Res<bool> {
        self.ensure_topic_branch_or_unborn("rebase")?;
        self.ensure_no_merge_in_progress("rebase")?;
        self.ensure_clean("rebase")?;
        let branch = self.head_branch()?;
        if !branch.starts_with("refs/heads/") {
            return Err("rebase: HEAD is not on a branch".into());
        }
        let repo = alt_repo::Repository::discover(&self.store.alt_dir)?;
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
        if !interactive && self.merge_bases(&[orig], &[onto])? == [onto] {
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

        // hand HEAD to the scratch ref, sitting on the new base
        let scratch = self.scratch_ref();
        let head_ref = self.head_ref.clone();
        self.commit_refs(
            "rebase",
            &[
                RefChange {
                    name: scratch.clone(),
                    old: None,
                    new: Some(RefTarget::Oid(onto)),
                },
                RefChange {
                    name: head_ref,
                    old: Some(RefTarget::Symbolic(branch.clone())),
                    new: Some(RefTarget::Symbolic(scratch)),
                },
            ],
        )?;
        let old = self.commit_entries(orig)?;
        let target = self.commit_entries(onto)?;
        self.checkout(&old, &target)?;
        let state = RebaseState {
            branch,
            orig,
            onto,
            current: None,
            todo,
        };
        self.save_rebase_state(&state)?;
        self.run_rebase(state, json, out)
    }

    /// `alt rebase --continue`: commit the resolved step, then go on.
    pub fn rebase_continue(&mut self, json: bool, out: &mut impl Write) -> Res<bool> {
        let mut state = self
            .rebase_state()?
            .ok_or("rebase --continue: no rebase in progress")?;
        if let Some((action, commit)) = state.current {
            if self.index()?.entries.iter().any(|e| e.stage() > 0) {
                return Err(
                    "rebase --continue: resolve the conflicts and `alt add` them first".into(),
                );
            }
            let tree = self.staged_tree()?;
            let head = self.rebase_head()?;
            if tree == self.commit_tree(head)? && !action.melds() {
                return Err(
                    "rebase --continue: no changes staged; if nothing is left of \
                            this commit, run `alt rebase --skip`"
                        .into(),
                );
            }
            self.apply_step(action, commit, tree)?;
            state.current = None;
            self.save_rebase_state(&state)?;
        }
        self.run_rebase(state, json, out)
    }

    /// `alt rebase --skip`: drop the stopped step and go on.
    pub fn rebase_skip(&mut self, json: bool, out: &mut impl Write) -> Res<bool> {
        let mut state = self
            .rebase_state()?
            .ok_or("rebase --skip: no rebase in progress")?;
        let head = self.rebase_head()?;
        self.reset_worktree_to(head)?;
        state.current = None;
        self.save_rebase_state(&state)?;
        self.run_rebase(state, json, out)
    }

    /// `alt rebase --abort`: back to the branch as it was; the branch never
    /// moved, so only HEAD, the index and the working tree change.
    pub fn rebase_abort(&mut self, out: &mut impl Write) -> Res<()> {
        let state = self
            .rebase_state()?
            .ok_or("rebase --abort: no rebase in progress")?;
        self.reset_worktree_to(state.orig)?;
        self.leave_scratch(&state.branch)?;
        self.clear_rebase_state()?;
        writeln!(out, "Rebase aborted.")?;
        Ok(())
    }

    fn run_rebase(
        &mut self,
        mut state: RebaseState,
        json: bool,
        out: &mut impl Write,
    ) -> Res<bool> {
        while !state.todo.is_empty() {
            let (action, commit) = state.todo.remove(0);
            if action == Action::Drop {
                self.save_rebase_state(&state)?;
                continue;
            }
            let head = self.rebase_head()?;
            match self.pick(head, commit)? {
                Picked::Conflicted(conflicts) => {
                    state.current = Some((action, commit));
                    self.save_rebase_state(&state)?;
                    self.report_rebase_stop(commit, &conflicts, json, out)?;
                    return Ok(true);
                }
                Picked::Clean(tree) if tree == self.commit_tree(head)? && !action.melds() => {
                    // its changes are already in the new base: nothing left to replay
                }
                Picked::Clean(tree) => self.apply_step(action, commit, tree)?,
            }
            self.save_rebase_state(&state)?;
        }
        let new = self.rebase_head()?;
        self.leave_scratch(&state.branch)?;
        if new != state.orig {
            self.commit_refs(
                "rebase",
                &[RefChange {
                    name: state.branch.clone(),
                    old: Some(RefTarget::Oid(state.orig)),
                    new: Some(RefTarget::Oid(new)),
                }],
            )?;
        }
        self.clear_rebase_state()?;
        if json {
            use crate::json::Json;
            let short = state
                .branch
                .strip_prefix("refs/heads/")
                .unwrap_or(&state.branch);
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
    /// checks the result out.
    fn pick(&mut self, head: ObjectId, commit: ObjectId) -> Res<Picked> {
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
        let tree = write_tree(&mut self.store.odb, &entries, self.store.algo)?;
        let old = index_entries(&self.index()?);
        self.checkout(&old, &entries)?;
        Ok(Picked::Clean(tree))
    }

    /// Records one replayed step whose tree is ready: a new commit for pick
    /// and reword, a replacement of the previous one for squash and fixup.
    fn apply_step(&mut self, action: Action, commit: ObjectId, tree: ObjectId) -> Res<()> {
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
        let author = parse_sig(&author_line)?;
        let id = self.id.clone();
        let (name, email) = id.sig();
        let me = Sig {
            name,
            email,
            when: (now_ms() / 1000) as i64,
            tz: "+0000",
        };
        let scratch = self.scratch_ref();
        self.record_commit(
            NewCommit {
                tree,
                parents: &parents,
                author: &author,
                committer: &me,
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

    /// Lists the commits the rebase will replay, oldest first: those reachable
    /// from `orig` but not from `onto`, without merges (git drops them too),
    /// parents before children, older committer time first among peers.
    fn commits_to_replay(&self, orig: ObjectId, onto: ObjectId) -> Res<Vec<ObjectId>> {
        let from_orig = self.ancestry(&[orig])?;
        let from_onto = self.ancestry(&[onto])?;
        let mine: std::collections::HashMap<ObjectId, Vec<ObjectId>> = from_orig
            .into_iter()
            .filter(|(c, _)| !from_onto.contains_key(c))
            .collect();
        let mut done = std::collections::HashSet::new();
        let mut order = Vec::new();
        while done.len() < mine.len() {
            let mut ready: Vec<(i64, ObjectId)> = Vec::new();
            for (c, parents) in &mine {
                if !done.contains(c)
                    && parents
                        .iter()
                        .all(|p| !mine.contains_key(p) || done.contains(p))
                {
                    ready.push((self.committer_time(*c)?, *c));
                }
            }
            ready.sort();
            let (_, next) = ready[0];
            done.insert(next);
            if mine[&next].len() < 2 {
                order.push(next);
            }
        }
        Ok(order)
    }

    fn edit_todo(
        &mut self,
        todo: &[(Action, ObjectId)],
        onto: ObjectId,
        orig: ObjectId,
    ) -> Res<Vec<(Action, ObjectId)>> {
        let mut text = String::new();
        for (action, c) in todo {
            let subject = self.read_commit_parts(*c)?.subject();
            text.push_str(&format!(
                "{} {} {subject}\n",
                action.word(),
                &c.to_string()[..7]
            ));
        }
        let (o, h) = (&onto.to_string()[..7], &orig.to_string()[..7]);
        text.push_str(&format!(
            "\n# Rebase {o}..{h} onto {o} ({} command{})\n",
            todo.len(),
            if todo.len() == 1 { "" } else { "s" }
        ));
        text.push_str(ed::TODO_HELP);
        let path = self.index_path.with_file_name("REBASE_TODO");
        std::fs::write(&path, text)?;
        ed::edit(&path, true)?;
        let edited = std::fs::read_to_string(&path)?;
        std::fs::remove_file(&path)?;
        let repo = alt_repo::Repository::discover(&self.store.alt_dir)?;
        ed::parse_todo(&edited, |rev| {
            repo.rev_parse(rev)?
                .ok_or_else(|| format!("rebase: unknown commit '{rev}' in the todo list").into())
        })
    }

    fn squash_message(&self, first: &str, next: &str) -> Res<String> {
        let text = format!(
            "# This is a combination of 2 commits.\n# This is the 1st commit message:\n\n{first}\n\
             # This is the commit message #2:\n\n{next}"
        );
        self.edited_message(&text)
    }

    /// Runs the editor on a commit message and applies git's cleanup.
    fn edited_message(&self, message: &str) -> Res<String> {
        let path = self.index_path.with_file_name("REBASE_MSG");
        std::fs::write(&path, message)?;
        ed::edit(&path, false)?;
        let edited = ed::cleanup(&std::fs::read_to_string(&path)?);
        std::fs::remove_file(&path)?;
        if edited.is_empty() {
            return Err("rebase: aborting because the commit message is empty".into());
        }
        Ok(edited)
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
        for p in conflicts {
            writeln!(out, "CONFLICT (content): Merge conflict in {p}")?;
        }
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
    fn scratch_ref(&self) -> String {
        format!("rebase/{}", self.workspace)
    }

    fn rebase_head(&self) -> Res<ObjectId> {
        Ok(self
            .store
            .refs
            .resolve(&self.scratch_ref())?
            .ok_or("rebase: the scratch ref is missing")?)
    }

    /// Points HEAD back at `branch` and drops the scratch ref.
    fn leave_scratch(&mut self, branch: &str) -> Res<()> {
        let scratch = self.scratch_ref();
        let at = self.store.refs.resolve(&scratch)?;
        let head_ref = self.head_ref.clone();
        self.commit_refs(
            "rebase",
            &[
                RefChange {
                    name: head_ref,
                    old: Some(RefTarget::Symbolic(scratch.clone())),
                    new: Some(RefTarget::Symbolic(branch.to_owned())),
                },
                RefChange {
                    name: scratch,
                    old: at.map(RefTarget::Oid),
                    new: None,
                },
            ],
        )?;
        Ok(())
    }

    /// Makes the index and working tree match `commit`, clearing whatever a
    /// stopped step left behind, conflicted paths included.
    fn reset_worktree_to(&mut self, commit: ObjectId) -> Res<()> {
        let mut known: Vec<WorkEntry> = Vec::new();
        for e in &self.index()?.entries {
            if known.last().is_none_or(|k: &WorkEntry| k.path != e.path) {
                known.push(WorkEntry {
                    path: e.path.clone(),
                    oid: e.oid,
                    mode: e.mode,
                });
            }
        }
        let target = self.commit_entries(commit)?;
        self.checkout(&known, &target)
    }

    fn committer_time(&self, commit: ObjectId) -> Res<i64> {
        let obj = self
            .store
            .odb
            .get(&commit)?
            .ok_or("commit missing from store")?;
        Ok(alt_git_codec::Commit::parse(&obj.data)?
            .committer_date()
            .unwrap_or(0))
    }

    fn read_commit_parts(&self, commit: ObjectId) -> Res<CommitParts> {
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

    fn rebase_state_path(&self) -> PathBuf {
        self.index_path.with_file_name("REBASE_STATE")
    }

    pub(super) fn rebase_in_progress(&self) -> bool {
        self.rebase_state_path().exists()
    }

    fn rebase_state(&self) -> Res<Option<RebaseState>> {
        let text = match std::fs::read_to_string(self.rebase_state_path()) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let corrupt = || "corrupt REBASE_STATE";
        let oid = |v: &str| v.parse::<ObjectId>().map_err(|_| corrupt());
        let (mut branch, mut orig, mut onto, mut current) = (None, None, None, None);
        let mut todo = Vec::new();
        for line in text.lines() {
            let (key, value) = line.split_once(' ').ok_or_else(corrupt)?;
            match key {
                "branch" => branch = Some(value.to_owned()),
                "orig" => orig = Some(oid(value)?),
                "onto" => onto = Some(oid(value)?),
                "current" => current = Some(parse_step_line(value, &oid)?),
                "todo" => todo.push(parse_step_line(value, &oid)?),
                _ => return Err(corrupt().into()),
            }
        }
        Ok(Some(RebaseState {
            branch: branch.ok_or_else(corrupt)?,
            orig: orig.ok_or_else(corrupt)?,
            onto: onto.ok_or_else(corrupt)?,
            current,
            todo,
        }))
    }

    fn save_rebase_state(&self, s: &RebaseState) -> Res<()> {
        let mut text = format!("branch {}\norig {}\nonto {}\n", s.branch, s.orig, s.onto);
        if let Some((a, c)) = s.current {
            text.push_str(&format!("current {} {c}\n", a.word()));
        }
        for (a, c) in &s.todo {
            text.push_str(&format!("todo {} {c}\n", a.word()));
        }
        let path = self.rebase_state_path();
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn clear_rebase_state(&self) -> Res<()> {
        match std::fs::remove_file(self.rebase_state_path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// An `alt commit` during a stopped rebase records that step itself;
    /// `alt rebase --continue` then goes on with the rest.
    pub(super) fn rebase_committed(&self) -> Res<()> {
        if let Some(mut state) = self.rebase_state()? {
            state.current = None;
            self.save_rebase_state(&state)?;
        }
        Ok(())
    }
}

/// `<action> <oid>` from the state file, trusted as written.
fn parse_step_line(
    value: &str,
    oid: &dyn Fn(&str) -> Result<ObjectId, &'static str>,
) -> Res<(Action, ObjectId)> {
    let (word, c) = value.split_once(' ').ok_or("corrupt REBASE_STATE")?;
    let action = [
        Action::Pick,
        Action::Reword,
        Action::Squash,
        Action::Fixup,
        Action::Drop,
    ]
    .into_iter()
    .find(|a| a.word() == word)
    .ok_or("corrupt REBASE_STATE")?;
    Ok((action, oid(c)?))
}

struct CommitParts {
    parents: Vec<ObjectId>,
    author: String,
    message: String,
}

impl CommitParts {
    fn subject(&self) -> String {
        self.message.lines().next().unwrap_or("").to_owned()
    }
}
