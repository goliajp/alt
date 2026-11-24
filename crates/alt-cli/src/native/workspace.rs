//! Parallel workspaces: create, remove, list, and the rule that a branch
//! is checked out in at most one of them.

use super::*;

mod rules;

pub(super) use rules::{check_workspace_name, one_workspace_per_branch, parse_workspace_marker};

impl NativeRepo<'_> {
    /// `alt workspace add <name> <path>`: create a parallel workspace whose
    /// working tree is `worktree`, checked out on `branch`. The HEAD is a
    /// per-workspace ref in the shared store, so it is transactional and
    /// undoable; the index and working tree are this workspace's alone.
    pub fn create_workspace(&mut self, name: &str, worktree: &Path, branch: &str) -> Res<()> {
        self.create_workspace_with(name, worktree, branch, None)
    }

    /// [`create_workspace`](Self::create_workspace), optionally creating
    /// `branch` at `new_branch_at` in the same ref transaction.
    fn create_workspace_with(
        &mut self,
        name: &str,
        worktree: &Path,
        branch: &str,
        new_branch_at: Option<ObjectId>,
    ) -> Res<()> {
        self.ensure_writable("workspace add")?;
        check_workspace_name(name)?;
        let head_ref = format!("workspaces/{name}/HEAD");
        if self.store.refs.get(&head_ref).is_some() {
            return Err(format!("a workspace named '{name}' already exists").into());
        }
        let branch_ref = format!("refs/heads/{branch}");
        let mut changes = Vec::new();
        let commit = match new_branch_at {
            Some(at) => {
                if self.store.refs.get(&branch_ref).is_some() {
                    return Err(crate::machine::Failure::refusal(
                        "precondition_failed",
                        format!("a branch named '{branch}' already exists"),
                    )
                    .into());
                }
                changes.push(RefChange {
                    name: branch_ref.clone(),
                    old: None,
                    new: Some(RefTarget::Oid(at)),
                });
                at
            }
            None => {
                if let Some((at, _)) = self.busy_at(&branch_ref, true)? {
                    let at = at.display();
                    let said = format!("'{branch}' is already used by worktree at '{at}'");
                    return Err(settings::fatal(said));
                }
                self.store.refs.resolve(&branch_ref)?.ok_or_else(|| {
                    crate::machine::Failure::refusal(
                        "precondition_failed",
                        format!("invalid reference: {branch}"),
                    )
                })?
            }
        };
        changes.push(RefChange {
            name: head_ref.clone(),
            old: None,
            new: Some(RefTarget::Symbolic(branch_ref)),
        });
        // fail before touching the disk; the ref store re-checks under its lock
        let refs: std::collections::BTreeMap<_, _> = self
            .store
            .refs
            .iter()
            .map(|(n, t)| (n.to_owned(), t.clone()))
            .collect();
        one_workspace_per_branch(&self.head_ref, &refs, &changes)?;

        // the working tree must live outside the repository: a tree nested
        // under another workspace's would show up there as untracked files.
        std::fs::create_dir_all(worktree)?;
        let abs = std::fs::canonicalize(worktree)?;
        let repo_root =
            std::fs::canonicalize(self.store.alt_dir.parent().unwrap_or(&self.store.alt_dir))?;
        if abs.starts_with(&repo_root) {
            return Err("workspace working tree must be outside the repository".into());
        }

        let ws_dir = self.store.alt_dir.join("workspaces").join(name);
        if ws_dir.exists() {
            return Err(
                "workspace registry already exists; inspect incomplete creation before retrying"
                    .into(),
            );
        }
        if abs.join(".alt").exists() || abs.join(".git").exists() {
            return Err("workspace destination already contains repository metadata".into());
        }
        // Check the destination before publishing either its marker or HEAD.
        let target = self.commit_entries(commit)?;
        let child = Coord {
            root: abs.clone(),
            workspace: name.to_owned(),
            head_ref,
            index_path: ws_dir.join("index"),
            prefix: String::new(),
        };
        let preview = child.clone();
        NativeRepo::attach(&mut *self.store, preview, self.id.clone(), self.request_id)
            .preflight_checkout(&[], &target)?;
        std::fs::create_dir_all(&ws_dir)?;
        crate::durable::write(
            ws_dir.join("meta"),
            abs.to_str().ok_or("non-utf8 worktree path")?,
        )?;
        // a `.alt` *file* in the working tree points back at the repo, so
        // commands run from inside it auto-select this workspace (git-worktree
        // style). scan_worktree skips `.alt` by name, so it is not content.
        crate::durable::write(
            abs.join(".alt"),
            format!(
                "{}\n{name}\n",
                repo_root.to_str().ok_or("non-utf8 repo path")?
            ),
        )?;

        // point this workspace's HEAD at the branch (one ref transaction);
        // if the store refuses it, the markers above must not outlive it
        if let Err(error) = self.commit_refs("workspace", &changes) {
            let cleanup = crate::durable::remove_file(abs.join(".alt"))
                .and_then(|()| crate::durable::remove_dir_all(&ws_dir));
            if let Err(cleanup) = cleanup {
                return Err(crate::machine::Failure::unknown(format!(
                    "{error}; workspace marker cleanup failed: {cleanup}"
                ))
                .into());
            }
            return Err(error);
        }

        // materialize the branch tree into the new working tree + index by
        // attaching a child view (sharing this store) and checking out from an
        // empty base
        let mut ws = NativeRepo::attach(&mut *self.store, child, self.id.clone(), self.request_id);
        let target = ws.commit_entries(commit)?;
        ws.checkout(&[], &target)?;
        Ok(())
    }

    /// `alt workspace remove <name>`: drop a named workspace's HEAD ref and
    /// control dir. The working-tree files are left in place (the caller owns
    /// them); the default workspace cannot be removed.
    pub fn remove_workspace(&mut self, name: &str) -> Res<()> {
        if name == DEFAULT_WORKSPACE {
            return Err("cannot remove the default workspace".into());
        }
        let head_ref = format!("workspaces/{name}/HEAD");
        let old = self
            .store
            .refs
            .get(&head_ref)
            .cloned()
            .ok_or_else(|| format!("no such workspace '{name}'"))?;
        self.commit_refs(
            "workspace",
            &[RefChange {
                name: head_ref,
                old: Some(old),
                new: None,
            }],
        )?;
        let ws_dir = self.store.alt_dir.join("workspaces").join(name);
        // drop the working tree's `.alt` marker so it no longer resolves
        let worktree = std::fs::read_to_string(ws_dir.join("meta"))?;
        match crate::durable::remove_file(PathBuf::from(worktree.trim()).join(".alt")) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if ws_dir.exists() {
            crate::durable::remove_dir_all(&ws_dir)?;
        }
        Ok(())
    }

    /// All workspaces: the default plus every registered named one, as
    /// `(name, working-tree path, is-current)`.
    pub fn list_workspaces(&self) -> Res<Vec<(String, PathBuf, bool)>> {
        let mut out = vec![(
            DEFAULT_WORKSPACE.to_owned(),
            self.store
                .alt_dir
                .parent()
                .unwrap_or(&self.store.alt_dir)
                .to_path_buf(),
            self.workspace == DEFAULT_WORKSPACE,
        )];
        let ws_root = self.store.alt_dir.join("workspaces");
        if let Ok(entries) = std::fs::read_dir(&ws_root) {
            let mut named: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            named.sort_by_key(|e| e.file_name());
            for entry in named {
                let name = entry.file_name().to_string_lossy().into_owned();
                let meta = entry.path().join("meta");
                if let Ok(worktree) = std::fs::read_to_string(&meta) {
                    out.push((
                        name.clone(),
                        PathBuf::from(worktree.trim()),
                        self.workspace == name,
                    ));
                }
            }
        }
        Ok(out)
    }

    /// `alt workspace add <name> <path> [branch]`: create the workspace on
    /// `branch`, or on a new branch `<name>` at the current commit, and
    /// report it.
    pub fn workspace_add(
        &mut self,
        name: &str,
        path: &Path,
        branch: Option<&str>,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let branch = match branch {
            Some(b) => {
                self.create_workspace(name, path, b)?;
                b.to_owned()
            }
            // like `git worktree add`: the current branch is checked out here
            // already, so the new workspace gets its own, at this commit
            None => {
                let at = self
                    .store
                    .refs
                    .resolve(&self.head_branch()?)?
                    .ok_or("no commit to start the workspace from")?;
                self.create_workspace_with(name, path, name, Some(at))?;
                name.to_owned()
            }
        };
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("workspace", Json::str(name)),
                    ("branch", Json::str(&branch)),
                    ("path", Json::str(path.to_string_lossy().as_bytes())),
                ],
            )?;
        } else {
            writeln!(
                out,
                "Created workspace '{name}' at {} on {branch}",
                path.display()
            )?;
        }
        Ok(())
    }

    /// `alt workspace remove <name>`: drop the workspace and report it.
    pub fn workspace_remove(&mut self, name: &str, json: bool, out: &mut impl Write) -> Res<()> {
        self.remove_workspace(name)?;
        if json {
            use crate::json::Json;
            crate::json::emit(out, vec![("removed", Json::str(name))])?;
        } else {
            writeln!(out, "Removed workspace '{name}'")?;
        }
        Ok(())
    }

    /// `alt workspace list`: the workspaces, human or JSON.
    pub fn workspace_list(&self, json: bool, out: &mut impl Write) -> Res<()> {
        let list = self.list_workspaces()?;
        if json {
            use crate::json::Json;
            let arr = list
                .iter()
                .map(|(name, path, current)| {
                    Json::Object(vec![
                        ("name", Json::str(name)),
                        ("path", Json::str(path.to_string_lossy().as_bytes())),
                        ("current", Json::Bool(*current)),
                    ])
                })
                .collect();
            crate::json::emit(out, vec![("workspaces", Json::Array(arr))])?;
        } else {
            for (name, path, current) in &list {
                let mark = if *current { "* " } else { "  " };
                writeln!(out, "{mark}{name}\t{}", path.display())?;
            }
        }
        Ok(())
    }
}
