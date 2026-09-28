//! Parallel workspaces: create, remove, list, and the rule that a branch
//! is checked out in at most one of them.

use std::collections::BTreeMap;

use super::*;

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
                    return Err(format!("a branch named '{branch}' already exists").into());
                }
                changes.push(RefChange {
                    name: branch_ref.clone(),
                    old: None,
                    new: Some(RefTarget::Oid(at)),
                });
                at
            }
            None => self
                .store
                .refs
                .resolve(&branch_ref)?
                .ok_or_else(|| format!("invalid reference: {branch}"))?,
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
        std::fs::create_dir_all(&ws_dir)?;
        std::fs::write(
            ws_dir.join("meta"),
            abs.to_str().ok_or("non-utf8 worktree path")?,
        )?;
        // a `.alt` *file* in the working tree points back at the repo, so
        // commands run from inside it auto-select this workspace (git-worktree
        // style). scan_worktree skips `.alt` by name, so it is not content.
        std::fs::write(
            abs.join(".alt"),
            format!(
                "{}\n{name}\n",
                repo_root.to_str().ok_or("non-utf8 repo path")?
            ),
        )?;

        // point this workspace's HEAD at the branch (one ref transaction);
        // if the store refuses it, the markers above must not outlive it
        if let Err(e) = self.commit_refs_unkeyed("workspace", &changes) {
            let _ = std::fs::remove_dir_all(&ws_dir);
            let _ = std::fs::remove_file(abs.join(".alt"));
            return Err(e);
        }

        // materialize the branch tree into the new working tree + index by
        // attaching a child view (sharing this store) and checking out from an
        // empty base
        let child = Coord {
            root: abs,
            workspace: name.to_owned(),
            head_ref,
            index_path: ws_dir.join("index"),
        };
        let mut ws = NativeRepo::attach(&mut *self.store, child, self.id.clone(), None);
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
        self.commit_refs_unkeyed(
            "workspace",
            &[RefChange {
                name: head_ref,
                old: Some(old),
                new: None,
            }],
        )?;
        let ws_dir = self.store.alt_dir.join("workspaces").join(name);
        // drop the working tree's `.alt` marker so it no longer resolves
        if let Ok(worktree) = std::fs::read_to_string(ws_dir.join("meta")) {
            let _ = std::fs::remove_file(PathBuf::from(worktree.trim()).join(".alt"));
        }
        if ws_dir.exists() {
            std::fs::remove_dir_all(&ws_dir)?;
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

/// A branch is checked out in at most one workspace, as in git. Moving or
/// deleting a branch another workspace has checked out would leave that
/// workspace's index and working tree describing a commit its HEAD no longer
/// names, so a commit there would silently undo the move; checking it out a
/// second time sets up the same trap. `own_head` is the HEAD ref of the
/// workspace making the change; `refs` is the state the change applies to.
pub(super) fn one_workspace_per_branch(
    own_head: &str,
    refs: &BTreeMap<String, RefTarget>,
    changes: &[RefChange],
) -> Result<(), String> {
    for c in changes {
        // the branch this change moves (any workspace but ours holding it is
        // a conflict) or checks out (any workspace but this one)
        let (branch, holder_ok) = if c.name.starts_with("refs/heads/") {
            (c.name.as_str(), own_head)
        } else if is_workspace_head(&c.name) {
            match &c.new {
                Some(RefTarget::Symbolic(b)) => (b.as_str(), c.name.as_str()),
                _ => continue,
            }
        } else {
            continue;
        };
        for (head, target) in refs {
            if head == holder_ok || !is_workspace_head(head) {
                continue;
            }
            if !matches!(target, RefTarget::Symbolic(b) if b == branch) {
                continue;
            }
            // that workspace lets go of the branch in this same transaction
            if changes
                .iter()
                .any(|o| o.name == *head && o.new.as_ref() != Some(target))
            {
                continue;
            }
            let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
            return Err(format!(
                "branch '{short}' is checked out in workspace '{}'",
                workspace_of(head)
            ));
        }
    }
    Ok(())
}

fn is_workspace_head(name: &str) -> bool {
    name == "HEAD" || (name.starts_with("workspaces/") && name.ends_with("/HEAD"))
}

fn workspace_of(head: &str) -> &str {
    head.strip_prefix("workspaces/")
        .and_then(|n| n.strip_suffix("/HEAD"))
        .unwrap_or(DEFAULT_WORKSPACE)
}

/// Reads a working tree's `.alt` marker file: line 1 is the repo root (the
/// directory holding the real `.alt`), line 2 is the workspace name.
pub(super) fn parse_workspace_marker(path: &Path) -> Res<(PathBuf, String)> {
    let content = std::fs::read_to_string(path)?;
    let mut lines = content.lines();
    let repo_root = lines.next().ok_or("malformed .alt workspace marker")?;
    let name = lines.next().ok_or("malformed .alt workspace marker")?;
    Ok((PathBuf::from(repo_root), name.to_owned()))
}

/// A workspace name: a single path segment (it becomes part of the ref name
/// `workspaces/<name>/HEAD` and a directory), so no slashes, dots, control or
/// special chars, and not the reserved default name.
pub(super) fn check_workspace_name(name: &str) -> Res<()> {
    let bad = name.is_empty()
        || name == DEFAULT_WORKSPACE
        || name.starts_with('.')
        || name.contains('/')
        || name.contains(['\\', ' ', '~', '^', ':', '?', '*', '[', '.'])
        || name.bytes().any(|b| b < 0x20 || b == 0x7f);
    if bad {
        return Err(format!("'{name}' is not a valid workspace name").into());
    }
    Ok(())
}

/// Walks up from `start` to the nearest repository and, when it is an alt
/// one (a `.alt` beside a `.git` counts as alt), resolves which
/// workspace applies, and returns the control dir plus the coordinates. An
/// explicit `workspace` name always wins. Otherwise the workspace is inferred:
/// under a repo root (a `.alt` directory) → the default workspace; inside a
/// named workspace's working tree (a `.alt` *file* pointing back at the repo,
/// git-worktree style) → that workspace.
pub fn resolve_workspace(start: &Path, workspace: Option<&str>) -> Res<(PathBuf, Coord)> {
    let mut dir: &Path = start;
    loop {
        let marker = dir.join(".alt");
        if marker.is_dir() {
            let coord = Coord::for_name(&marker, workspace.unwrap_or(DEFAULT_WORKSPACE))?;
            return Ok((marker, coord));
        }
        if marker.is_file() {
            let (repo_root, name) = parse_workspace_marker(&marker)?;
            let alt_dir = repo_root.join(".alt");
            let coord = Coord::for_name(&alt_dir, workspace.unwrap_or(&name))?;
            return Ok((alt_dir, coord));
        }
        // the nearest repository wins, as in git: a git repository nested
        // in an alt working tree is its own, not part of the outer one
        if dir.join(".git").exists() {
            return Err("not an alt repository (inside a git repository)".into());
        }
        dir = dir
            .parent()
            .ok_or("not an alt repository (no .alt found)")?;
    }
}
