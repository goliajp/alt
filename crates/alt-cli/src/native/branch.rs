//! `alt branch`: the branches listed, one made, one deleted; and what a
//! new branch follows where it starts at a branch of a remote.

use super::*;

mod moved;

/// The branch of a remote that a local branch follows.
pub(super) struct Followed {
    pub(super) remote: String,
    /// the branch as the remote has it, without `refs/heads/`
    pub(super) branch: String,
}

impl NativeRepo<'_> {
    /// `alt branch`: list branches (no args), create one (`name`) at
    /// `start`, HEAD where none is given, or delete one (`delete`).
    pub fn branch(
        &mut self,
        name: Option<String>,
        start: Option<String>,
        delete: Option<String>,
        remotes: bool,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        match (name, delete) {
            (_, Some(target)) if remotes => self.delete_ref(&format!("refs/remotes/{target}"), out),
            (_, Some(target)) => self.delete_branch(&target, out),
            (Some(new), None) => self.create_branch(&new, start.as_deref(), out),
            (None, None) => self.list_branches(json, out),
        }
    }

    fn list_branches(&self, json: bool, out: &mut impl Write) -> Res<()> {
        let current = self.head_branch()?;
        if json {
            return self.list_branches_json(&current, out);
        }
        for (name, _) in self.store.refs.iter() {
            if let Some(short) = name.strip_prefix("refs/heads/") {
                let mark = if name == current { "* " } else { "  " };
                writeln!(out, "{mark}{short}")?;
            }
        }
        Ok(())
    }

    /// `branch --json`: `{schema_version, current, branches:[{name,current,oid}]}`
    /// — every local branch with its tip oid and which one HEAD is on.
    fn list_branches_json(&self, current: &str, out: &mut impl Write) -> Res<()> {
        use crate::json::Json;
        let mut branches = Vec::new();
        for (name, _) in self.store.refs.iter() {
            if let Some(short) = name.strip_prefix("refs/heads/") {
                let oid = match self.store.refs.resolve(name)? {
                    Some(o) => Json::str(o.to_string()),
                    None => Json::Null,
                };
                branches.push(Json::Object(vec![
                    ("name", Json::str(short)),
                    ("current", Json::Bool(name == current)),
                    ("oid", oid),
                ]));
            }
        }
        let short = current.strip_prefix("refs/heads/").unwrap_or(current);
        let doc = Json::Object(vec![
            ("schema_version", Json::Num(1)),
            ("current", Json::str(short)),
            ("branches", Json::Array(branches)),
        ]);
        doc.write(out)?;
        out.write_all(b"\n")?;
        Ok(())
    }

    pub(super) fn create_branch(
        &mut self,
        name: &str,
        start: Option<&str>,
        out: &mut impl Write,
    ) -> Res<()> {
        check_branch_name(name)?;
        let full = format!("refs/heads/{name}");
        self.ensure_no_branch(name)?;
        let (commit, follows) = match start {
            Some(start) => (self.branch_point(start)?, self.followed(start)?),
            None => {
                let head = self.store.refs.resolve(&self.head_branch()?)?;
                let head = head.ok_or("cannot create a branch before the first commit")?;
                (head, None)
            }
        };
        self.commit_refs_only(
            "branch",
            &[RefChange {
                name: full,
                old: None,
                new: Some(RefTarget::Oid(commit)),
            }],
        )?;
        writeln!(out, "branch '{name}' created at {commit}")?;
        if let Some(followed) = follows {
            writeln!(out, "{}", self.follow(name, &followed)?)?;
        }
        Ok(())
    }

    /// Refuses, in git's words, a new branch by a `name` one has.
    pub(super) fn ensure_no_branch(&self, name: &str) -> Res<()> {
        if self.store.refs.get(&format!("refs/heads/{name}")).is_none() {
            return Ok(());
        }
        let said = format!("a branch named '{name}' already exists");
        let refusal = crate::machine::Failure::refusal("precondition_failed", said);
        Err(Box::new(crate::machine::Failure {
            exit_code: 128,
            ..refusal
        }))
    }

    /// The commit `start` names for `alt branch` to make a branch at, or
    /// git's words for what names none.
    pub(super) fn branch_point(&self, start: &str) -> Res<ObjectId> {
        if let Some(commit) = self.commit_named(start)? {
            return Ok(commit);
        }
        let repo = self.store.git_view()?;
        let spec = pathspec::at_head(&self.head_ref, start);
        let Some(named) = repo.rev_parse(&spec)? else {
            return Err(settings::fatal(format!(
                "not a valid object name: '{start}'"
            )));
        };
        let (object, peeled) = repo.peel(named)?;
        let kind = match peeled.kind {
            ObjectKind::Blob => "blob",
            ObjectKind::Tree => "tree",
            ObjectKind::Tag => "tag",
            ObjectKind::Commit => "commit",
        };
        let said = format!(
            "error: object {object} is a {kind}, not a commit
\
             fatal: not a valid branch point: '{start}'\n"
        );
        Err(said::refused(&said, 128))
    }

    /// The remote-tracking branch `start` names, where it names one of a
    /// remote that is known: the remote, and the branch as the remote
    /// has it. A branch made there follows it, as by git.
    pub(super) fn followed(&self, start: &str) -> Res<Option<Followed>> {
        let tried = [
            start.to_owned(),
            format!("refs/{start}"),
            format!("refs/tags/{start}"),
            format!("refs/heads/{start}"),
            format!("refs/remotes/{start}"),
        ];
        let found = tried
            .iter()
            .find(|name| self.store.refs.get(name).is_some());
        let Some(rest) = found.and_then(|name| name.strip_prefix("refs/remotes/")) else {
            return Ok(None);
        };
        let remotes = self.read_remotes()?;
        let there = |remote: &&Remote| {
            let branch = rest.strip_prefix(remote.name.as_str());
            branch.is_some_and(|branch| branch.starts_with('/'))
        };
        let of = remotes.iter().filter(there).max_by_key(|r| r.name.len());
        Ok(of.map(|remote| Followed {
            remote: remote.name.clone(),
            branch: rest[remote.name.len() + 1..].to_owned(),
        }))
    }

    /// Has the branch `name` follow `followed`, in the configuration alt
    /// keeps, and returns git's line for it.
    pub(super) fn follow(&mut self, name: &str, followed: &Followed) -> Res<String> {
        let repo = self.store.git_view()?;
        let Followed { remote, branch } = followed;
        crate::config_cmd::set(&repo, &format!("branch.{name}.remote"), remote)?;
        let merge = format!("refs/heads/{branch}");
        crate::config_cmd::set(&repo, &format!("branch.{name}.merge"), &merge)?;
        Ok(format!(
            "branch '{name}' set up to track '{remote}/{branch}'."
        ))
    }

    fn delete_branch(&mut self, name: &str, out: &mut impl Write) -> Res<()> {
        let full = format!("refs/heads/{name}");
        if full == self.head_branch()? {
            return Err(format!("cannot delete branch '{name}': it is the current branch").into());
        }
        self.delete_ref(&full, out)?;
        // what the branch followed goes with it, as with git
        crate::config_cmd::drop_section(&self.store.git_view()?, "branch", name)
    }

    /// Deletes `full` (a branch or remote-tracking ref) in one undoable op.
    fn delete_ref(&mut self, full: &str, out: &mut impl Write) -> Res<()> {
        let short = full
            .strip_prefix("refs/heads/")
            .or_else(|| full.strip_prefix("refs/remotes/"))
            .unwrap_or(full);
        let old = self
            .store
            .refs
            .get(full)
            .cloned()
            .ok_or_else(|| format!("branch '{short}' not found"))?;
        self.commit_refs_only(
            "branch",
            &[RefChange {
                name: full.to_owned(),
                old: Some(old),
                new: None,
            }],
        )?;
        writeln!(out, "deleted branch '{short}'")?;
        Ok(())
    }
}

impl NativeRepo<'_> {
    /// The branch this workspace was on before the one it is on, by its
    /// short name; `None` where it was on no other.
    pub fn branch_left(&self) -> Res<Option<String>> {
        Ok(self.store.git_view()?.branch_before(&self.head_ref, 0)?)
    }
}
