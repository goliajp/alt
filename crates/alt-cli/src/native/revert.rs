//! `alt revert`: commits that undo earlier commits, as git makes them. A
//! sequence stops at the first conflict; its state lives next to the
//! workspace's index until `--continue` or `--abort`.

use super::commit::NewCommit;
use super::*;

/// A state file line by line, before `orig` is known to be present.
struct RevertStateParts {
    current: Option<ObjectId>,
    todo: Vec<ObjectId>,
    mainline: Option<usize>,
    orig: Option<ObjectId>,
}

/// What a stopped revert sequence remembers.
pub(super) struct RevertState {
    /// The commit whose revert stopped on a conflict; `None` once that revert
    /// has been committed (by `alt commit`) and only the rest is left.
    current: Option<ObjectId>,
    /// Commits still to revert, in order.
    todo: Vec<ObjectId>,
    /// The parent to revert against, for merge commits (1-based, as `-m`).
    mainline: Option<usize>,
    /// Where the branch stood before the sequence, for `--abort`.
    orig: ObjectId,
}

enum Reverted {
    Committed(ObjectId),
    Conflicted(Vec<BString>),
}

impl NativeRepo<'_> {
    /// `alt revert <rev>...`: one new commit per revision, each undoing it.
    /// Returns whether it stopped on a conflict.
    pub fn revert(
        &mut self,
        revs: &[String],
        mainline: Option<usize>,
        json: bool,
        out: &mut impl Write,
    ) -> Res<bool> {
        self.ensure_topic_branch_or_unborn("revert")?;
        self.ensure_no_merge_in_progress("revert")?;
        self.ensure_clean("revert")?;
        let repo = alt_repo::Repository::discover(&self.store.alt_dir)?;
        let todo = revs
            .iter()
            .map(|r| {
                repo.rev_parse(r)?
                    .ok_or_else(|| format!("bad revision '{r}'").into())
            })
            .collect::<Res<Vec<ObjectId>>>()?;
        let orig = self.branch_tip("revert")?;
        self.run_reverts(todo, mainline, orig, json, out)
    }

    /// `alt revert --continue`: commit the resolved revert, then go on with
    /// the rest of the sequence.
    pub fn revert_continue(&mut self, json: bool, out: &mut impl Write) -> Res<bool> {
        let state = self
            .revert_state()?
            .ok_or("revert --continue: no revert in progress")?;
        if let Some(current) = state.current {
            if self.index()?.entries.iter().any(|e| e.stage() > 0) {
                return Err(
                    "revert --continue: resolve the conflicts and `alt add` them first".into(),
                );
            }
            let head = self.branch_tip("revert --continue")?;
            let tree = self.staged_tree()?;
            let msg = self.revert_message(current, state.mainline)?;
            let commit = self.commit_revert(head, tree, &msg)?;
            self.report_revert(commit, &msg, json, out)?;
        }
        self.run_reverts(state.todo, state.mainline, state.orig, json, out)
    }

    /// `alt revert --abort`: put the branch, index and working tree back to
    /// where they were before the sequence started.
    pub fn revert_abort(&mut self, out: &mut impl Write) -> Res<()> {
        let state = self
            .revert_state()?
            .ok_or("revert --abort: no revert in progress")?;
        let branch = self.head_branch()?;
        let head = self.branch_tip("revert --abort")?;
        // every path the index knows, conflicted ones included, so checkout
        // clears whatever the stopped revert left in the working tree
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
        let target = self.commit_entries(state.orig)?;
        self.checkout(&known, &target)?;
        if head != state.orig {
            self.commit_refs(
                "revert",
                &[RefChange {
                    name: branch,
                    old: Some(RefTarget::Oid(head)),
                    new: Some(RefTarget::Oid(state.orig)),
                }],
            )?;
        }
        self.clear_revert_state()?;
        writeln!(out, "Revert aborted.")?;
        Ok(())
    }

    fn run_reverts(
        &mut self,
        todo: Vec<ObjectId>,
        mainline: Option<usize>,
        orig: ObjectId,
        json: bool,
        out: &mut impl Write,
    ) -> Res<bool> {
        for (i, &target) in todo.iter().enumerate() {
            match self.revert_one(target, mainline)? {
                Reverted::Committed(commit) => {
                    let msg = self.revert_message(target, mainline)?;
                    self.report_revert(commit, &msg, json, out)?;
                }
                Reverted::Conflicted(conflicts) => {
                    self.save_revert_state(&RevertState {
                        current: Some(target),
                        todo: todo[i + 1..].to_vec(),
                        mainline,
                        orig,
                    })?;
                    if json {
                        self.report_merge(true, out, "conflicted", None, &conflicts, "")?;
                    } else {
                        for p in &conflicts {
                            writeln!(out, "CONFLICT (content): Merge conflict in {p}")?;
                        }
                        let short = &target.to_string()[..7];
                        writeln!(
                            out,
                            "error: could not revert {short}; fix the conflicts, `alt add` them \
                             and run `alt revert --continue` (or `alt revert --abort`)"
                        )?;
                    }
                    return Ok(true);
                }
            }
        }
        self.clear_revert_state()?;
        Ok(false)
    }

    /// Reverts one commit onto the branch tip: a three-way merge whose base is
    /// the commit and whose other side is its (mainline) parent.
    fn revert_one(&mut self, target: ObjectId, mainline: Option<usize>) -> Res<Reverted> {
        let head = self.branch_tip("revert")?;
        let parent = self.revert_parent(target, mainline)?;
        let base = self.commit_entries(target)?;
        let ours = self.commit_entries(head)?;
        let theirs = match parent {
            Some(p) => self.commit_entries(p)?,
            None => Vec::new(), // reverting a root commit removes what it added
        };
        let label = format!("parent of {}", &target.to_string()[..7]);
        let resolved = self.merge_trees(&base, &ours, &theirs, &label)?;
        self.store.odb.flush()?;
        if resolved.iter().any(|r| r.conflicted) {
            self.write_conflicted(&resolved)?;
            let conflicts = resolved
                .iter()
                .filter(|r| r.conflicted)
                .map(|r| r.path.clone())
                .collect();
            return Ok(Reverted::Conflicted(conflicts));
        }
        let entries: Vec<WorkEntry> = resolved.into_iter().filter_map(|r| r.entry).collect();
        let tree = write_tree(&mut self.store.odb, &entries, self.store.algo)?;
        let old = index_entries(&self.index()?);
        self.checkout(&old, &entries)?;
        let msg = self.revert_message(target, mainline)?;
        Ok(Reverted::Committed(self.commit_revert(head, tree, &msg)?))
    }

    /// Records a revert commit on top of `head`, refusing one that changes
    /// nothing (git stops there too).
    fn commit_revert(&mut self, head: ObjectId, tree: ObjectId, msg: &str) -> Res<ObjectId> {
        let head_tree = self.commit_tree(head)?;
        if head_tree == tree {
            return Err("revert: nothing to commit, the revert changes nothing".into());
        }
        let branch = self.head_branch()?;
        let id = self.id.clone();
        let (name, email) = id.sig();
        let me = Sig {
            name,
            email,
            when: (now_ms() / 1000) as i64,
            tz: "+0000",
        };
        self.record_commit(
            NewCommit {
                tree,
                parents: &[head],
                author: &me,
                committer: &me,
                message: msg,
            },
            &branch,
            Some(head),
            "revert",
        )
    }

    /// The parent a revert of `target` goes back to: its only parent, or for
    /// a merge the one `-m` names. Git's rules for when `-m` is required or
    /// refused apply.
    fn revert_parent(&self, target: ObjectId, mainline: Option<usize>) -> Res<Option<ObjectId>> {
        let short = &target.to_string()[..7];
        let parents = self.commit_parents(target)?;
        match (parents.len(), mainline) {
            (0, None) => Ok(None),
            (1, None) => Ok(Some(parents[0])),
            (n, None) => Err(format!(
                "revert: commit {short} is a merge ({n} parents) but no -m option was given"
            )
            .into()),
            (n, Some(_)) if n < 2 => Err(format!(
                "revert: mainline was specified but commit {short} is not a merge"
            )
            .into()),
            (n, Some(m)) => parents
                .get(m.wrapping_sub(1))
                .copied()
                .map(Some)
                .ok_or_else(|| {
                    format!("revert: commit {short} does not have parent {m} (it has {n})").into()
                }),
        }
    }

    /// Git's revert message: `Revert "<subject>"` (or `Reapply "…"` when the
    /// subject is itself a revert) and a line naming the reverted commit.
    fn revert_message(&self, target: ObjectId, mainline: Option<usize>) -> Res<String> {
        let obj = self
            .store
            .odb
            .get(&target)?
            .ok_or("commit missing from store")?;
        let commit = alt_git_codec::Commit::parse(&obj.data)?;
        let message = String::from_utf8_lossy(commit.message()).into_owned();
        let subject = message.lines().next().unwrap_or("");
        let title = match subject
            .strip_prefix("Revert \"")
            .and_then(|s| s.strip_suffix('"'))
        {
            Some(inner) => format!("Reapply \"{inner}\""),
            None => format!("Revert \"{subject}\""),
        };
        let body = match mainline {
            Some(_) => {
                let parent = self
                    .revert_parent(target, mainline)?
                    .ok_or("merge without parents")?;
                format!("This reverts commit {target}, reversing\nchanges made to {parent}.")
            }
            None => format!("This reverts commit {target}."),
        };
        Ok(format!("{title}\n\n{body}\n"))
    }

    fn report_revert(
        &self,
        commit: ObjectId,
        msg: &str,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let branch = self.head_branch()?;
        let short = branch.strip_prefix("refs/heads/").unwrap_or(&branch);
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("branch", Json::str(short)),
                    ("commit", Json::str(commit.to_string())),
                ],
            )?;
        } else {
            let title = msg.lines().next().unwrap_or("");
            writeln!(out, "[{short} {}] {title}", &commit.to_string()[..7])?;
        }
        Ok(())
    }

    fn branch_tip(&self, verb: &str) -> Res<ObjectId> {
        let branch = self.head_branch()?;
        Ok(self
            .store
            .refs
            .resolve(&branch)?
            .ok_or_else(|| format!("{verb}: the branch has no commits yet"))?)
    }

    pub(super) fn commit_tree(&self, commit: ObjectId) -> Res<ObjectId> {
        let obj = self
            .store
            .odb
            .get(&commit)?
            .ok_or("commit missing from store")?;
        Ok(alt_git_codec::Commit::parse(&obj.data)?
            .tree()
            .ok_or("commit without a tree")?)
    }

    pub(super) fn commit_parents(&self, commit: ObjectId) -> Res<Vec<ObjectId>> {
        let obj = self
            .store
            .odb
            .get(&commit)?
            .ok_or("commit missing from store")?;
        Ok(alt_git_codec::Commit::parse(&obj.data)?.parents().collect())
    }

    fn revert_state_path(&self) -> PathBuf {
        self.index_path.with_file_name("REVERT_STATE")
    }

    /// The stopped sequence, if one is waiting.
    pub(super) fn revert_state(&self) -> Res<Option<RevertState>> {
        let text = match std::fs::read_to_string(self.revert_state_path()) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let corrupt = || "corrupt REVERT_STATE";
        let mut state = RevertStateParts {
            current: None,
            todo: Vec::new(),
            mainline: None,
            orig: None,
        };
        for line in text.lines() {
            let (key, value) = line.split_once(' ').ok_or_else(corrupt)?;
            match key {
                "current" => state.current = Some(value.parse().map_err(|_| corrupt())?),
                "todo" => state.todo.push(value.parse().map_err(|_| corrupt())?),
                "mainline" => state.mainline = Some(value.parse().map_err(|_| corrupt())?),
                "orig" => state.orig = Some(value.parse().map_err(|_| corrupt())?),
                _ => return Err(corrupt().into()),
            }
        }
        Ok(Some(RevertState {
            current: state.current,
            todo: state.todo,
            mainline: state.mainline,
            orig: state.orig.ok_or_else(corrupt)?,
        }))
    }

    fn save_revert_state(&self, s: &RevertState) -> Res<()> {
        let mut text = format!("orig {}\n", s.orig);
        if let Some(c) = s.current {
            text.push_str(&format!("current {c}\n"));
        }
        if let Some(m) = s.mainline {
            text.push_str(&format!("mainline {m}\n"));
        }
        for t in &s.todo {
            text.push_str(&format!("todo {t}\n"));
        }
        std::fs::write(self.revert_state_path(), text)?;
        Ok(())
    }

    fn clear_revert_state(&self) -> Res<()> {
        match std::fs::remove_file(self.revert_state_path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// An `alt commit` in the middle of a stopped revert concludes that one
    /// revert; the rest of the sequence waits for `alt revert --continue`.
    pub(super) fn revert_committed(&self) -> Res<()> {
        if let Some(mut state) = self.revert_state()? {
            state.current = None;
            self.save_revert_state(&state)?;
        }
        Ok(())
    }
}
