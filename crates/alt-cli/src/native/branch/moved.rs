//! A branch put at another commit (`branch -f`) or given another name
//! (`branch -m`, `-M`).

use super::*;

impl NativeRepo<'_> {
    /// Where the workspace that has the branch `full` checked out is, or
    /// that rebases or bisects it: any workspace, or with `own` left out
    /// any but this one.
    pub(in crate::native) fn held_at(&self, full: &str, own: bool) -> Res<Option<PathBuf>> {
        for (workspace, path, current) in self.list_workspaces()? {
            if current && !own {
                continue;
            }
            let head = match workspace.as_str() {
                DEFAULT_WORKSPACE => "HEAD".to_owned(),
                named => format!("workspaces/{named}/HEAD"),
            };
            let on = self.store.refs.get(&head);
            let on_it = matches!(on, Some(RefTarget::Symbolic(branch)) if branch == full);
            if on_it || self.busy_with(&workspace, full)?.is_some() {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    /// What the workspace `workspace` took its HEAD off the branch `full`
    /// for and has not ended: `rebased` or `bisected`, in git's words.
    fn busy_with(&self, workspace: &str, full: &str) -> Res<Option<&'static str>> {
        let index = Coord::for_name(&self.store.alt_dir, workspace)?.index_path;
        for (file, doing) in [("REBASE_STATE", "rebased"), ("BISECT_STATE", "bisected")] {
            let text = match std::fs::read_to_string(index.with_file_name(file)) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            if text
                .lines()
                .any(|line| line.strip_prefix("branch ") == Some(full))
            {
                return Ok(Some(doing));
            }
        }
        Ok(None)
    }

    /// Where the branch `full` is being rebased or bisected, and which:
    /// in any workspace, or with `own` left out any but this one.
    pub(in crate::native) fn busy_at(
        &self,
        full: &str,
        own: bool,
    ) -> Res<Option<(PathBuf, &'static str)>> {
        for (workspace, path, current) in self.list_workspaces()? {
            if current && !own {
                continue;
            }
            if let Some(doing) = self.busy_with(&workspace, full)? {
                return Ok(Some((path, doing)));
            }
        }
        Ok(None)
    }

    /// Refuses, in git's words, to put elsewhere a branch a workspace
    /// has checked out: its index and files would be of a commit its
    /// HEAD no longer names.
    fn ensure_not_held(&self, name: &str) -> Res<()> {
        let Some(at) = self.held_at(&format!("refs/heads/{name}"), true)? else {
            return Ok(());
        };
        Err(settings::fatal(format!(
            "cannot force update the branch '{name}' used by worktree at '{}'",
            at.display()
        )))
    }

    /// Refuses to put elsewhere, or to rename, a branch the flow model
    /// protects, as a commit made on it directly is refused.
    pub(in crate::native) fn ensure_may_move(&self, name: &str) -> Res<()> {
        let model = self.flow_model();
        let flow = format!("refs/heads/{}", model.develop);
        let protected = name == model.main || name == model.develop;
        if self.id.protected_override || !protected || self.store.refs.get(&flow).is_none() {
            return Ok(());
        }
        let said = format!(
            "refusing to move protected branch '{name}': it takes commits through \
             `alt flow` alone (override with ALT_PROTECTED_OVERRIDE=1 only for emergency \
             recovery)"
        );
        Err(crate::machine::Failure::refusal("precondition_failed", said).into())
    }

    /// `alt branch -f <name> [<start>]`: the branch at the commit `start`
    /// names, HEAD where none is given, whether or not there is one of
    /// the name.
    pub fn branch_force(
        &mut self,
        name: &str,
        start: Option<&str>,
        out: &mut impl Write,
    ) -> Res<()> {
        let full = format!("refs/heads/{name}");
        let Some(old) = self.store.refs.get(&full).cloned() else {
            return self.create_branch(name, start, out);
        };
        self.ensure_not_held(name)?;
        let (commit, follows) = match start {
            Some(start) => (self.branch_point(start)?, self.followed(start)?),
            None => {
                let head = self.store.refs.resolve(&self.head_branch()?)?;
                let head = head
                    .ok_or_else(|| settings::fatal("not a valid object name: 'HEAD'".to_owned()))?;
                (head, None)
            }
        };
        let new = RefTarget::Oid(commit);
        if old != new {
            self.ensure_may_move(name)?;
            let moved = RefChange {
                name: full,
                old: Some(old),
                new: Some(new),
            };
            self.commit_refs_only("branch", &[moved])?;
        }
        writeln!(out, "branch '{name}' reset to {commit}")?;
        if let Some(followed) = follows {
            writeln!(out, "{}", self.follow(name, &followed)?)?;
        }
        Ok(())
    }

    /// A switch to the branch `name`, `full`, may begin: nothing is under
    /// way here, and no other workspace rebases or bisects the branch.
    pub(in crate::native) fn ensure_may_switch_to(&self, name: &str, full: &str) -> Res<()> {
        self.ensure_no_merge_in_progress("switch")?;
        if let Some((at, _)) = self.busy_at(full, false)? {
            let said = format!("'{name}' is already used by worktree at '{}'", at.display());
            return Err(settings::fatal(said));
        }
        Ok(())
    }

    /// The branch `branch -m` renames, `old` or the current one: its ref,
    /// its short name and what it points at. Refused, in git's words,
    /// where there is none or a rebase or a bisect has HEAD off it.
    fn renamed_from(&self, old: Option<&str>) -> Res<(String, String, RefTarget)> {
        let from = match old {
            Some(old) => format!("refs/heads/{old}"),
            None => self.head_branch()?,
        };
        let Some(old) = from.strip_prefix("refs/heads/").map(str::to_owned) else {
            let said = "cannot rename the current branch while not on any";
            return Err(settings::fatal(said.to_owned()));
        };
        let Some(at) = self.store.refs.get(&from).cloned() else {
            return Err(settings::fatal(format!("no branch named '{old}'")));
        };
        if let Some((at, doing)) = self.busy_at(&from, true)? {
            let said = format!("branch {from} is being {doing} at {}", at.display());
            return Err(settings::fatal(said));
        }
        Ok((from, old, at))
    }

    /// `alt branch -m [<old>] <new>`, and `-M` with `over`: the branch
    /// under another name, with what it follows, in every workspace
    /// that has it checked out. `-M` takes the place of a branch of the
    /// new name.
    pub fn branch_rename(
        &mut self,
        old: Option<&str>,
        new: &str,
        over: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        check_branch_name(new)?;
        let (from, old, at) = self.renamed_from(old)?;
        if old == new {
            return Ok(());
        }
        let to = format!("refs/heads/{new}");
        let taken = self.store.refs.get(&to).cloned();
        if taken.is_some() {
            match over {
                true => self.ensure_not_held(new)?,
                false => self.ensure_no_branch(new)?,
            }
            self.ensure_may_move(new)?;
        }
        self.ensure_may_move(&old)?;
        let mut changes = vec![
            RefChange {
                name: from.clone(),
                old: Some(at.clone()),
                new: None,
            },
            RefChange {
                name: to.clone(),
                old: taken,
                new: Some(at),
            },
        ];
        // every HEAD on the branch follows it to its new name
        let on_it = RefTarget::Symbolic(from);
        let heads = self
            .store
            .refs
            .iter()
            .filter(|(_, target)| **target == on_it);
        let heads: Vec<String> = heads.map(|(name, _)| name.to_owned()).collect();
        changes.extend(heads.into_iter().map(|name| RefChange {
            name,
            old: Some(on_it.clone()),
            new: Some(RefTarget::Symbolic(to.clone())),
        }));
        self.commit_refs_only("branch", &changes)?;
        crate::config_cmd::rename_section(&self.store.git_view()?, "branch", &old, new)?;
        writeln!(out, "branch '{old}' renamed to '{new}'")?;
        Ok(())
    }
}
