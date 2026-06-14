//! Native `.alt` repository commands: `init`, `add`, `commit`, `status`.
//! These wire the alt-worktree write primitives and alt-refs op log into a
//! dogfoodable commit loop. The control dir is `<root>/.alt`; the index is
//! git index v2 at `.alt/index`; HEAD and branches are native refs.

use std::io::Write;
use std::path::{Path, PathBuf};

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_git_index::{Index, IndexEntry};
use alt_odb::NativeOdb;
use alt_refs::{RefChange, RefStore, RefTarget};
use alt_worktree::{
    ChangeKind, Sig, WorkEntry, flatten_tree, index_entries, scan_worktree, status, write_commit,
    write_tree,
};
use bstr::{BString, ByteSlice};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn actor(verb: &str) -> String {
    format!(
        "cli/{verb}@{}",
        std::env::var("USER").as_deref().unwrap_or("unknown")
    )
}

/// `alt init [dir]`: create an empty native repo with an unborn HEAD → main.
pub fn init(dir: Option<PathBuf>, out: &mut impl Write) -> Res<()> {
    let root = dir.unwrap_or_else(|| PathBuf::from("."));
    let alt_dir = root.join(".alt");
    if alt_dir.exists() {
        return Err(format!("{} already exists", alt_dir.display()).into());
    }
    std::fs::create_dir_all(&root)?;
    let mut odb = NativeOdb::open(&alt_dir)?;
    odb.flush()?;
    let mut refs = RefStore::open(&alt_dir)?;
    refs.commit(
        &actor("init"),
        now_ms(),
        &[RefChange {
            name: "HEAD".to_owned(),
            old: None,
            new: Some(RefTarget::Symbolic("refs/heads/main".to_owned())),
        }],
    )?;
    save_index(&alt_dir.join("index"), &empty_index())?;
    writeln!(
        out,
        "Initialized empty alt repository in {}",
        alt_dir.display()
    )?;
    Ok(())
}

/// The default workspace's name — its HEAD stays the bare `HEAD` ref and its
/// index stays at `.alt/index`, so existing repos are unchanged.
const DEFAULT_WORKSPACE: &str = "default";

/// An opened native repo, bound to one workspace. The odb, branch refs, and
/// op log are shared across workspaces; the HEAD ref, index, and working tree
/// are per-workspace, so N agents can work in parallel without colliding.
pub struct NativeRepo {
    root: PathBuf,
    alt_dir: PathBuf,
    odb: NativeOdb,
    refs: RefStore,
    algo: HashAlgo,
    /// This workspace's name (`default` for the repo-root workspace).
    workspace: String,
    /// The ref naming this workspace's HEAD: `HEAD` for the default workspace,
    /// `workspaces/<name>/HEAD` for a named one (kept out of `refs/heads/`).
    head_ref: String,
    /// This workspace's index file.
    index_path: PathBuf,
}

impl NativeRepo {
    /// Walks up from `start` for a directory holding `.alt`, opening its
    /// default workspace (working tree = the directory that holds `.alt`).
    pub fn discover(start: &Path) -> Res<Self> {
        Self::discover_workspace(start, None)
    }

    /// Like `discover`, but selects a workspace. An explicit `workspace` name
    /// always wins. Otherwise the workspace is inferred from where `start`
    /// lands: under a repo root (a `.alt` directory) → the default workspace;
    /// inside a named workspace's working tree (a `.alt` *file* pointing back
    /// at the repo, git-worktree style) → that workspace.
    pub fn discover_workspace(start: &Path, workspace: Option<&str>) -> Res<Self> {
        let mut dir: &Path = start;
        loop {
            let marker = dir.join(".alt");
            if marker.is_dir() {
                return match workspace {
                    Some(name) => Self::open_workspace(dir, name),
                    None => Self::open_default(dir),
                };
            }
            if marker.is_file() {
                let (repo_root, name) = parse_workspace_marker(&marker)?;
                return Self::open_workspace(&repo_root, workspace.unwrap_or(&name));
            }
            dir = dir
                .parent()
                .ok_or("not an alt repository (no .alt found)")?;
        }
    }

    /// Opens the default workspace of the repo rooted at `repo_root`.
    fn open_default(repo_root: &Path) -> Res<Self> {
        let alt_dir = repo_root.join(".alt");
        let index_path = alt_dir.join("index");
        Self::open_with(
            repo_root.to_path_buf(),
            alt_dir,
            DEFAULT_WORKSPACE.to_owned(),
            "HEAD".to_owned(),
            index_path,
        )
    }

    /// Shared constructor for any workspace.
    fn open_with(
        root: PathBuf,
        alt_dir: PathBuf,
        workspace: String,
        head_ref: String,
        index_path: PathBuf,
    ) -> Res<Self> {
        Ok(Self {
            odb: NativeOdb::open(&alt_dir)?,
            refs: RefStore::open(&alt_dir)?,
            root,
            alt_dir,
            algo: HashAlgo::Sha1,
            workspace,
            head_ref,
            index_path,
        })
    }

    /// Opens a specific workspace of the repo at `repo_root` (the directory
    /// that holds `.alt`). The `default` workspace is the repo root; a named
    /// workspace's working tree comes from its registry `meta`.
    pub fn open_workspace(repo_root: &Path, name: &str) -> Res<Self> {
        if name == DEFAULT_WORKSPACE {
            return Self::open_default(repo_root);
        }
        let alt_dir = repo_root.join(".alt");
        let ws_dir = alt_dir.join("workspaces").join(name);
        let worktree = std::fs::read_to_string(ws_dir.join("meta"))
            .map_err(|_| format!("no such workspace '{name}'"))?;
        Self::open_with(
            PathBuf::from(worktree.trim()),
            alt_dir,
            name.to_owned(),
            format!("workspaces/{name}/HEAD"),
            ws_dir.join("index"),
        )
    }

    /// `alt workspace add <name> <path>`: create a parallel workspace whose
    /// working tree is `worktree`, checked out on `branch`. The HEAD is a
    /// per-workspace ref in the shared store, so it is transactional and
    /// undoable; the index and working tree are this workspace's alone.
    pub fn create_workspace(&mut self, name: &str, worktree: &Path, branch: &str) -> Res<()> {
        check_workspace_name(name)?;
        let head_ref = format!("workspaces/{name}/HEAD");
        if self.refs.get(&head_ref).is_some() {
            return Err(format!("a workspace named '{name}' already exists").into());
        }
        let branch_ref = format!("refs/heads/{branch}");
        let commit = self
            .refs
            .resolve(&branch_ref)?
            .ok_or_else(|| format!("invalid reference: {branch}"))?;

        // the working tree must live outside the repository: a tree nested
        // under another workspace's would show up there as untracked files.
        std::fs::create_dir_all(worktree)?;
        let abs = std::fs::canonicalize(worktree)?;
        let repo_root = std::fs::canonicalize(self.alt_dir.parent().unwrap_or(&self.alt_dir))?;
        if abs.starts_with(&repo_root) {
            return Err("workspace working tree must be outside the repository".into());
        }

        let ws_dir = self.alt_dir.join("workspaces").join(name);
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

        // point this workspace's HEAD at the branch (one ref transaction)
        self.refs.commit(
            &actor("workspace"),
            now_ms(),
            &[RefChange {
                name: head_ref.clone(),
                old: None,
                new: Some(RefTarget::Symbolic(branch_ref)),
            }],
        )?;

        // materialize the branch tree into the new working tree + index by
        // opening the workspace and checking out from an empty base
        let mut ws = Self::open_with(
            abs,
            self.alt_dir.clone(),
            name.to_owned(),
            head_ref,
            ws_dir.join("index"),
        )?;
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
            .refs
            .get(&head_ref)
            .cloned()
            .ok_or_else(|| format!("no such workspace '{name}'"))?;
        self.refs.commit(
            &actor("workspace"),
            now_ms(),
            &[RefChange {
                name: head_ref,
                old: Some(old),
                new: None,
            }],
        )?;
        let ws_dir = self.alt_dir.join("workspaces").join(name);
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
            self.alt_dir.parent().unwrap_or(&self.alt_dir).to_path_buf(),
            self.workspace == DEFAULT_WORKSPACE,
        )];
        let ws_root = self.alt_dir.join("workspaces");
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

    /// `alt workspace add <name> <path> [branch]`: create the workspace (on
    /// `branch`, defaulting to the current branch) and report it.
    pub fn workspace_add(
        &mut self,
        name: &str,
        path: &Path,
        branch: Option<&str>,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let cur = self.head_branch()?;
        let branch = match branch {
            Some(b) => b.to_owned(),
            None => cur.strip_prefix("refs/heads/").unwrap_or(&cur).to_owned(),
        };
        self.create_workspace(name, path, &branch)?;
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

    fn index(&self) -> Res<Index> {
        match Index::open(&self.index_path, self.algo) {
            Ok(i) => Ok(i),
            Err(alt_git_index::IndexError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(empty_index())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// `alt add <paths>`: stage the given paths (or everything for `.`),
    /// updating the index to match the working tree.
    pub fn add(&mut self, paths: &[String], json: bool, out: &mut impl Write) -> Res<()> {
        let scan = scan_worktree(&self.root, self.algo)?;
        let staging_all = paths.iter().any(|p| p == ".");

        // start from the current stage-0 entries (or empty when restaging all)
        let mut entries: Vec<IndexEntry> = if staging_all {
            Vec::new()
        } else {
            self.index()?
                .entries
                .into_iter()
                .filter(|e| e.stage() == 0)
                .collect()
        };

        let mut staged = 0;
        let targets: Vec<BString> = if staging_all {
            scan.iter().map(|w| w.path.clone()).collect()
        } else {
            paths.iter().map(|p| BString::from(p.as_str())).collect()
        };
        for rel in &targets {
            entries.retain(|e| &e.path != rel);
            if let Some(w) = scan.iter().find(|w| &w.path == rel) {
                self.odb.put(w.oid, ObjectKind::Blob, &self.read_for(w)?)?;
                entries.push(self.make_entry(w)?);
                staged += 1;
            } // a path that vanished from the tree is dropped (staged deletion)
        }

        self.odb.flush()?;
        save_index(
            &self.index_path,
            &Index {
                version: 2,
                entries,
                extensions: Vec::new(),
            },
        )?;
        if json {
            crate::json::emit(out, vec![("staged", crate::json::Json::Num(staged as i64))])?;
        } else {
            writeln!(out, "staged {staged} path(s)")?;
        }
        Ok(())
    }

    fn read_for(&self, w: &WorkEntry) -> Res<Vec<u8>> {
        let abs = self
            .root
            .join(w.path.to_path().map_err(|_| "non-utf8 path")?);
        if w.mode == 0o120000 {
            Ok(std::fs::read_link(&abs)?
                .as_os_str()
                .as_encoded_bytes()
                .to_vec())
        } else {
            Ok(std::fs::read(&abs)?)
        }
    }

    fn make_entry(&self, w: &WorkEntry) -> Res<IndexEntry> {
        let abs = self
            .root
            .join(w.path.to_path().map_err(|_| "non-utf8 path")?);
        let meta = std::fs::symlink_metadata(&abs)?;
        Ok(stat_entry(&meta, w))
    }

    /// `alt commit -m <msg>`: write a tree + commit from the index, advance
    /// the current branch in one ref transaction.
    pub fn commit(&mut self, message: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let index = self.index()?;
        let staged = index_entries(&index);
        if staged.is_empty() {
            return Err("nothing to commit (empty index)".into());
        }
        let tree = write_tree(&mut self.odb, &staged, self.algo)?;

        let branch = self.head_branch()?;
        let parent = self.refs.resolve(&branch)?;
        let parents: Vec<ObjectId> = parent.into_iter().collect();

        let when = (now_ms() / 1000) as i64;
        let (name, email) = identity();
        let sig = Sig {
            name: &name,
            email: &email,
            when,
            tz: "+0000",
        };
        let msg = if message.ends_with('\n') {
            message.to_owned()
        } else {
            format!("{message}\n")
        };
        let commit = write_commit(&mut self.odb, tree, &parents, &sig, &sig, &msg, self.algo)?;
        self.odb.flush()?;

        self.refs.commit(
            &actor("commit"),
            now_ms(),
            &[RefChange {
                name: branch.clone(),
                old: parent.map(RefTarget::Oid),
                new: Some(RefTarget::Oid(commit)),
            }],
        )?;
        let short = branch.strip_prefix("refs/heads/").unwrap_or(&branch);
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("branch", Json::str(short)),
                    ("commit", Json::str(commit.to_string())),
                    ("tree", Json::str(tree.to_string())),
                ],
            )?;
        } else {
            writeln!(out, "[{short}] {commit}")?;
        }
        Ok(())
    }

    /// `alt status`: staged / unstaged / untracked against HEAD and the index,
    /// plus any unmerged (conflicted) paths left by a merge. With `json`, emits
    /// the stable structured schema instead of the human view (VISION §4 A1).
    pub fn status(&self, json: bool, out: &mut impl Write) -> Res<()> {
        let branch = self.head_branch()?;
        let head = self.head_entries()?;
        let raw = self.index()?;
        let unmerged: std::collections::BTreeSet<BString> = raw
            .entries
            .iter()
            .filter(|e| e.stage() > 0)
            .map(|e| e.path.clone())
            .collect();
        let index = index_entries(&raw);
        let worktree = scan_worktree(&self.root, self.algo)?;
        let mut st = status(&head, &index, &worktree);
        // unmerged paths are reported in their own section, not as
        // staged/unstaged/untracked noise driven by the missing stage-0 entry
        st.staged.retain(|(p, _)| !unmerged.contains(p));
        st.unstaged.retain(|(p, _)| !unmerged.contains(p));
        st.untracked.retain(|p| !unmerged.contains(p));

        let short = branch.strip_prefix("refs/heads/").unwrap_or(&branch);
        if json {
            return render_status_json(out, short, &st, &unmerged);
        }
        writeln!(out, "On branch {short}")?;
        let mark = |k: ChangeKind| match k {
            ChangeKind::Added => "new file",
            ChangeKind::Modified => "modified",
            ChangeKind::Deleted => "deleted",
        };
        if !unmerged.is_empty() {
            writeln!(out, "Unmerged paths:")?;
            for p in &unmerged {
                writeln!(out, "\tboth modified:   {p}")?;
            }
        }
        if !st.staged.is_empty() {
            writeln!(out, "Changes to be committed:")?;
            for (p, k) in &st.staged {
                writeln!(out, "\t{}:   {p}", mark(*k))?;
            }
        }
        if !st.unstaged.is_empty() {
            writeln!(out, "Changes not staged for commit:")?;
            for (p, k) in &st.unstaged {
                writeln!(out, "\t{}:   {p}", mark(*k))?;
            }
        }
        if !st.untracked.is_empty() {
            writeln!(out, "Untracked files:")?;
            for p in &st.untracked {
                writeln!(out, "\t{p}")?;
            }
        }
        if unmerged.is_empty()
            && st.staged.is_empty()
            && st.unstaged.is_empty()
            && st.untracked.is_empty()
        {
            writeln!(out, "nothing to commit, working tree clean")?;
        }
        Ok(())
    }

    /// The HEAD commit's tree flattened to entries (empty when unborn).
    fn head_entries(&self) -> Res<Vec<WorkEntry>> {
        match self.refs.resolve(&self.head_branch()?)? {
            Some(commit) => self.commit_entries(commit),
            None => Ok(Vec::new()),
        }
    }

    /// A commit's tree flattened to path-sorted entries.
    fn commit_entries(&self, commit: ObjectId) -> Res<Vec<WorkEntry>> {
        let obj = self.odb.get(&commit)?.ok_or("commit missing from store")?;
        let tree = alt_git_codec::Commit::parse(&obj.data)?
            .tree()
            .ok_or("commit has no tree")?;
        Ok(flatten_tree(&self.odb, tree, self.algo)?)
    }

    /// `alt diff` (index → working tree) or `alt diff --cached` (HEAD →
    /// index): a git-style unified diff of the tracked changes. With `json`,
    /// emits the structured per-file/per-hunk schema (VISION §4 A1).
    pub fn diff(&self, cached: bool, json: bool, out: &mut impl Write) -> Res<()> {
        // old side is always read from the object store; the new side is the
        // index (cached) or the live working tree (default).
        let (old, new, new_on_disk, show_added) = if cached {
            (
                self.head_entries()?,
                index_entries(&self.index()?),
                false,
                true,
            )
        } else {
            let index = index_entries(&self.index()?);
            let work = scan_worktree(&self.root, self.algo)?;
            (index, work, true, false)
        };

        let changes: Vec<_> = alt_worktree::changes(&old, &new)
            .into_iter()
            // a working-tree file with no index entry is untracked, not a diff
            .filter(|ch| ch.old.is_some() || show_added)
            .collect();

        if json {
            return self.diff_json(&changes, new_on_disk, out);
        }
        let mut buf = Vec::new();
        for ch in &changes {
            self.emit_file_diff(&mut buf, ch, new_on_disk)?;
        }
        out.write_all(&buf)?;
        Ok(())
    }

    /// Builds the `diff --json` document: `{schema_version, files:[…]}`, one
    /// entry per changed file with its oids/modes, a `binary` flag, and the
    /// structured hunks (empty for binary files).
    fn diff_json(
        &self,
        changes: &[alt_worktree::Change],
        new_on_disk: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        use crate::json::Json;
        let mut files = Vec::with_capacity(changes.len());
        for ch in changes {
            let old_bytes = match ch.old {
                Some(w) => self.blob_bytes(w.oid)?,
                None => Vec::new(),
            };
            let new_bytes = match ch.new {
                Some(w) if new_on_disk => self.read_for(w)?,
                Some(w) => self.blob_bytes(w.oid)?,
                None => Vec::new(),
            };
            let status = match (ch.old, ch.new) {
                (None, _) => "added",
                (_, None) => "deleted",
                _ => "modified",
            };
            let binary = alt_diff::is_binary(&old_bytes) || alt_diff::is_binary(&new_bytes);
            let hunks = if binary {
                Vec::new()
            } else {
                alt_diff::hunks(&old_bytes, &new_bytes, 3)
                    .iter()
                    .map(hunk_json)
                    .collect()
            };
            let oid = |w: Option<&WorkEntry>| match w {
                Some(w) => Json::str(w.oid.to_string()),
                None => Json::Null,
            };
            let mode = |w: Option<&WorkEntry>| match w {
                Some(w) => Json::str(format!("{:06o}", w.mode)),
                None => Json::Null,
            };
            files.push(Json::Object(vec![
                ("path", Json::str(ch.path)),
                ("status", Json::str(status)),
                ("old_oid", oid(ch.old)),
                ("new_oid", oid(ch.new)),
                ("old_mode", mode(ch.old)),
                ("new_mode", mode(ch.new)),
                ("binary", Json::Bool(binary)),
                ("hunks", Json::Array(hunks)),
            ]));
        }
        let doc = Json::Object(vec![
            ("schema_version", Json::Num(1)),
            ("files", Json::Array(files)),
        ]);
        doc.write(out)?;
        out.write_all(b"\n")?;
        Ok(())
    }

    /// Writes one file's `diff --git` stanza (header + hunks, or a binary
    /// notice) to `buf`.
    fn emit_file_diff(
        &self,
        buf: &mut Vec<u8>,
        ch: &alt_worktree::Change,
        new_on_disk: bool,
    ) -> Res<()> {
        let path = ch.path;
        let old_bytes = match ch.old {
            Some(w) => self.blob_bytes(w.oid)?,
            None => Vec::new(),
        };
        let new_bytes = match ch.new {
            Some(w) if new_on_disk => self.read_for(w)?,
            Some(w) => self.blob_bytes(w.oid)?,
            None => Vec::new(),
        };

        buf.extend_from_slice(format!("diff --git a/{path} b/{path}\n").as_bytes());
        match (ch.old, ch.new) {
            (None, Some(w)) => {
                buf.extend_from_slice(format!("new file mode {:06o}\n", w.mode).as_bytes());
            }
            (Some(w), None) => {
                buf.extend_from_slice(format!("deleted file mode {:06o}\n", w.mode).as_bytes());
            }
            (Some(o), Some(n)) if o.mode != n.mode => {
                buf.extend_from_slice(format!("old mode {:06o}\n", o.mode).as_bytes());
                buf.extend_from_slice(format!("new mode {:06o}\n", n.mode).as_bytes());
            }
            _ => {}
        }
        let o7 = abbrev(ch.old.map(|w| w.oid));
        let n7 = abbrev(ch.new.map(|w| w.oid));
        buf.extend_from_slice(format!("index {o7}..{n7}\n").as_bytes());

        if alt_diff::is_binary(&old_bytes) || alt_diff::is_binary(&new_bytes) {
            buf.extend_from_slice(
                format!("Binary files a/{path} and b/{path} differ\n").as_bytes(),
            );
            return Ok(());
        }

        match ch.old {
            Some(_) => buf.extend_from_slice(format!("--- a/{path}\n").as_bytes()),
            None => buf.extend_from_slice(b"--- /dev/null\n"),
        }
        match ch.new {
            Some(_) => buf.extend_from_slice(format!("+++ b/{path}\n").as_bytes()),
            None => buf.extend_from_slice(b"+++ /dev/null\n"),
        }
        alt_diff::write_unified(buf, &old_bytes, &new_bytes, 3);
        Ok(())
    }

    fn blob_bytes(&self, oid: ObjectId) -> Res<Vec<u8>> {
        Ok(self.odb.get(&oid)?.ok_or("object missing from store")?.data)
    }

    fn head_branch(&self) -> Res<String> {
        match self.refs.get(&self.head_ref) {
            Some(RefTarget::Symbolic(b)) => Ok(b.clone()),
            Some(RefTarget::Oid(_)) => Err("detached HEAD is not supported yet".into()),
            None => Ok("refs/heads/main".to_owned()),
        }
    }

    /// `alt branch`: list branches (no args), create one at HEAD (`name`),
    /// or delete one (`delete`).
    pub fn branch(
        &mut self,
        name: Option<String>,
        delete: Option<String>,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        match (name, delete) {
            (_, Some(target)) => self.delete_branch(&target, out),
            (Some(new), None) => self.create_branch(&new, out),
            (None, None) => self.list_branches(json, out),
        }
    }

    fn list_branches(&self, json: bool, out: &mut impl Write) -> Res<()> {
        let current = self.head_branch()?;
        if json {
            return self.list_branches_json(&current, out);
        }
        for (name, _) in self.refs.iter() {
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
        for (name, _) in self.refs.iter() {
            if let Some(short) = name.strip_prefix("refs/heads/") {
                let oid = match self.refs.resolve(name)? {
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

    fn create_branch(&mut self, name: &str, out: &mut impl Write) -> Res<()> {
        check_branch_name(name)?;
        let full = format!("refs/heads/{name}");
        if self.refs.get(&full).is_some() {
            return Err(format!("a branch named '{name}' already exists").into());
        }
        let commit = self
            .refs
            .resolve(&self.head_branch()?)?
            .ok_or("cannot create a branch before the first commit")?;
        self.refs.commit(
            &actor("branch"),
            now_ms(),
            &[RefChange {
                name: full,
                old: None,
                new: Some(RefTarget::Oid(commit)),
            }],
        )?;
        writeln!(out, "branch '{name}' created at {commit}")?;
        Ok(())
    }

    fn delete_branch(&mut self, name: &str, out: &mut impl Write) -> Res<()> {
        let full = format!("refs/heads/{name}");
        if full == self.head_branch()? {
            return Err(format!("cannot delete branch '{name}': it is the current branch").into());
        }
        let old = self
            .refs
            .get(&full)
            .cloned()
            .ok_or_else(|| format!("branch '{name}' not found"))?;
        self.refs.commit(
            &actor("branch"),
            now_ms(),
            &[RefChange {
                name: full,
                old: Some(old),
                new: None,
            }],
        )?;
        writeln!(out, "deleted branch '{name}'")?;
        Ok(())
    }

    /// `alt switch <name>`: point HEAD at branch `name` and materialize its
    /// tree into the working tree. `-c` creates the branch first. Switching
    /// to an existing branch requires a clean tree (no staged/unstaged
    /// changes) so no uncommitted work is lost.
    pub fn switch(
        &mut self,
        name: &str,
        create: bool,
        json: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        let full = format!("refs/heads/{name}");
        let current = self.head_branch()?;

        // emit either the human line or `{schema_version, branch, result}`
        let report = |out: &mut dyn Write, result: &'static str, human: &str| -> Res<()> {
            if json {
                use crate::json::Json;
                crate::json::emit(
                    out,
                    vec![("branch", Json::str(name)), ("result", Json::str(result))],
                )?;
            } else {
                writeln!(out, "{human}")?;
            }
            Ok(())
        };

        if create {
            check_branch_name(name)?;
            if self.refs.get(&full).is_some() {
                return Err(format!("a branch named '{name}' already exists").into());
            }
            // a new branch starts at the current commit (if any); the working
            // tree and index carry over unchanged, so no checkout is needed.
            if let Some(commit) = self.refs.resolve(&current)? {
                self.refs.commit(
                    &actor("branch"),
                    now_ms(),
                    &[RefChange {
                        name: full.clone(),
                        old: None,
                        new: Some(RefTarget::Oid(commit)),
                    }],
                )?;
            }
            self.move_head(&current, &full)?;
            return report(
                out,
                "created",
                &format!("Switched to a new branch '{name}'"),
            );
        }

        if self.refs.get(&full).is_none() {
            return Err(format!("invalid reference: {name}").into());
        }
        if full == current {
            return report(out, "already_on", &format!("Already on '{name}'"));
        }
        self.ensure_clean("switch")?;

        let target = match self.refs.resolve(&full)? {
            Some(commit) => self.commit_entries(commit)?,
            None => Vec::new(), // unborn target: tree becomes empty
        };
        let old = index_entries(&self.index()?);
        self.checkout(&old, &target)?;
        self.move_head(&current, &full)?;
        report(out, "switched", &format!("Switched to branch '{name}'"))
    }

    /// Refuses `action` if any tracked file has staged or unstaged changes, so
    /// a tree-rewriting operation never clobbers uncommitted work.
    fn ensure_clean(&self, action: &str) -> Res<()> {
        let head = self.head_entries()?;
        let index = index_entries(&self.index()?);
        let worktree = scan_worktree(&self.root, self.algo)?;
        let st = status(&head, &index, &worktree);
        if !st.staged.is_empty() || !st.unstaged.is_empty() {
            return Err(format!(
                "your local changes would be overwritten by {action}; commit them first"
            )
            .into());
        }
        Ok(())
    }

    /// Materializes `target` into the working tree, removing tracked files in
    /// `old` that are absent from `target`, and rewrites the index to match.
    fn checkout(&mut self, old: &[WorkEntry], target: &[WorkEntry]) -> Res<()> {
        use std::collections::HashSet;
        let old_paths: HashSet<&BString> = old.iter().map(|e| &e.path).collect();
        let target_paths: HashSet<&BString> = target.iter().map(|e| &e.path).collect();

        // validate first: an untracked file in the way of a target file is a
        // collision — refuse before touching anything.
        for t in target {
            if !old_paths.contains(&t.path) && self.abs(&t.path)?.symlink_metadata().is_ok() {
                return Err(format!(
                    "untracked working tree file '{}' would be overwritten by switch",
                    t.path
                )
                .into());
            }
        }

        for o in old {
            if !target_paths.contains(&o.path) {
                let abs = self.abs(&o.path)?;
                if abs.symlink_metadata().is_ok() {
                    std::fs::remove_file(&abs)?;
                    self.prune_empty_dirs(abs.parent());
                }
            }
        }

        let mut entries = Vec::with_capacity(target.len());
        for t in target {
            self.materialize(t)?;
            entries.push(self.make_entry(t)?);
        }
        save_index(
            &self.index_path,
            &Index {
                version: 2,
                entries,
                extensions: Vec::new(),
            },
        )?;
        Ok(())
    }

    /// Writes one tree entry to the working tree (regular file, executable,
    /// or symlink), creating parent dirs and replacing whatever was there.
    fn materialize(&self, w: &WorkEntry) -> Res<()> {
        let abs = self.abs(&w.path)?;
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if abs.symlink_metadata().is_ok() {
            std::fs::remove_file(&abs)?;
        }
        let obj = self.odb.get(&w.oid)?.ok_or("blob missing from store")?;
        if w.mode == 0o120000 {
            symlink_to(&obj.data, &abs)?;
        } else {
            std::fs::write(&abs, &obj.data)?;
            set_exec(&abs, w.mode == 0o100755)?;
        }
        Ok(())
    }

    /// Removes now-empty directories from `dir` up toward the root.
    fn prune_empty_dirs(&self, dir: Option<&Path>) {
        let mut cur = dir;
        while let Some(d) = cur {
            if d == self.root || std::fs::remove_dir(d).is_err() {
                break; // reached the root or a non-empty directory
            }
            cur = d.parent();
        }
    }

    fn abs(&self, rel: &BString) -> Res<PathBuf> {
        Ok(self.root.join(rel.to_path().map_err(|_| "non-utf8 path")?))
    }

    /// Moves HEAD's symbolic target from `from` to `to` in one ref op.
    fn move_head(&mut self, from: &str, to: &str) -> Res<()> {
        let old = self.refs.get(&self.head_ref).cloned();
        self.refs.commit(
            &actor("switch"),
            now_ms(),
            &[RefChange {
                name: self.head_ref.clone(),
                old: old.or_else(|| Some(RefTarget::Symbolic(from.to_owned()))),
                new: Some(RefTarget::Symbolic(to.to_owned())),
            }],
        )?;
        Ok(())
    }

    /// `alt merge <branch>`: three-way merge `branch` into the current branch.
    /// Returns whether the merge stopped in conflict. Fast-forwards when our
    /// branch is strictly behind; otherwise writes a two-parent merge commit
    /// (clean) or leaves conflict markers + unmerged index entries and makes
    /// no commit (conflicting). A clean working tree is required.
    pub fn merge(&mut self, branch_name: &str, json: bool, out: &mut impl Write) -> Res<bool> {
        let their_ref = format!("refs/heads/{branch_name}");
        let theirs = self
            .refs
            .resolve(&their_ref)?
            .ok_or_else(|| format!("merge: {branch_name} - not something we can merge"))?;
        let cur = self.head_branch()?;
        let ours = self
            .refs
            .resolve(&cur)?
            .ok_or("cannot merge into an unborn branch")?;
        self.ensure_clean("merge")?;

        match self.compute_merge(ours, theirs, branch_name)? {
            MergeOutcome::UpToDate => {
                self.report_merge(json, out, "up_to_date", None, &[], "Already up to date.")?;
                Ok(false)
            }
            MergeOutcome::FastForward(commit) => {
                let target = self.commit_entries(commit)?;
                let old = index_entries(&self.index()?);
                self.checkout(&old, &target)?;
                self.advance_branch(&cur, ours, commit)?;
                self.report_merge(
                    json,
                    out,
                    "fast_forward",
                    Some(commit),
                    &[],
                    &format!("Fast-forward to {commit}"),
                )?;
                Ok(false)
            }
            MergeOutcome::Merged { commit, entries } => {
                let old = index_entries(&self.index()?);
                self.checkout(&old, &entries)?;
                self.advance_branch(&cur, ours, commit)?;
                self.report_merge(
                    json,
                    out,
                    "merged",
                    Some(commit),
                    &[],
                    "Merge made by the 'ort' strategy.",
                )?;
                Ok(false)
            }
            MergeOutcome::Conflicted(resolved) => {
                self.write_conflicted(&resolved)?;
                let conflicts: Vec<BString> = resolved
                    .iter()
                    .filter(|r| r.conflicted)
                    .map(|r| r.path.clone())
                    .collect();
                if json {
                    self.report_merge(true, out, "conflicted", None, &conflicts, "")?;
                } else {
                    for p in &conflicts {
                        writeln!(out, "CONFLICT (content): Merge conflict in {p}")?;
                    }
                    writeln!(
                        out,
                        "Automatic merge failed; fix conflicts and then commit the result."
                    )?;
                }
                Ok(true)
            }
        }
    }

    /// Emits one merge outcome: the human line, or `{schema_version, result,
    /// commit?, conflicts}` for `--json`.
    fn report_merge(
        &self,
        json: bool,
        out: &mut impl Write,
        result: &'static str,
        commit: Option<ObjectId>,
        conflicts: &[BString],
        human: &str,
    ) -> Res<()> {
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("result", Json::str(result)),
                    (
                        "commit",
                        match commit {
                            Some(c) => Json::str(c.to_string()),
                            None => Json::Null,
                        },
                    ),
                    (
                        "conflicts",
                        Json::Array(conflicts.iter().map(Json::str).collect()),
                    ),
                ],
            )?;
        } else {
            writeln!(out, "{human}")?;
        }
        Ok(())
    }

    /// Computes how merging `theirs` into `ours` resolves, writing any merged
    /// blobs / tree / merge commit to the object store but touching no refs or
    /// working tree. The caller applies the outcome (advancing refs, checking
    /// out, or recording a conflict) — which lets both `alt merge` and the
    /// atomic `flow finish` share one merge engine.
    fn compute_merge(
        &mut self,
        ours: ObjectId,
        theirs: ObjectId,
        label: &str,
    ) -> Res<MergeOutcome> {
        let base = self.merge_base(ours, theirs)?;
        if base == Some(theirs) {
            return Ok(MergeOutcome::UpToDate);
        }
        if base == Some(ours) {
            return Ok(MergeOutcome::FastForward(theirs));
        }

        let base_entries = match base {
            Some(b) => self.commit_entries(b)?,
            None => Vec::new(), // unrelated histories: empty base
        };
        let ours_entries = self.commit_entries(ours)?;
        let theirs_entries = self.commit_entries(theirs)?;
        let resolved = self.merge_trees(&base_entries, &ours_entries, &theirs_entries, label)?;
        self.odb.flush()?;

        if resolved.iter().any(|r| r.conflicted) {
            return Ok(MergeOutcome::Conflicted(resolved));
        }

        let entries: Vec<WorkEntry> = resolved.iter().filter_map(|r| r.entry.clone()).collect();
        let tree = write_tree(&mut self.odb, &entries, self.algo)?;
        let when = (now_ms() / 1000) as i64;
        let (name, email) = identity();
        let sig = Sig {
            name: &name,
            email: &email,
            when,
            tz: "+0000",
        };
        let msg = format!("Merge branch '{label}'\n");
        let commit = write_commit(
            &mut self.odb,
            tree,
            &[ours, theirs],
            &sig,
            &sig,
            &msg,
            self.algo,
        )?;
        self.odb.flush()?;
        Ok(MergeOutcome::Merged { commit, entries })
    }

    /// `alt flow init`: create `develop` off `main` (or the current branch's
    /// commit) and switch to it, in one ref transaction.
    pub fn flow_init(&mut self, json: bool, out: &mut impl Write) -> Res<()> {
        use crate::json::Json;
        let model = alt_flow::BranchModel::default();
        let dev_ref = format!("refs/heads/{}", model.develop);
        if self.refs.get(&dev_ref).is_some() {
            if json {
                crate::json::emit(
                    out,
                    vec![
                        ("develop", Json::str(model.develop)),
                        ("already_initialized", Json::Bool(true)),
                    ],
                )?;
            } else {
                writeln!(out, "flow already initialized (branch '{}')", model.develop)?;
            }
            return Ok(());
        }
        let main_ref = format!("refs/heads/{}", model.main);
        let start = match self.refs.resolve(&main_ref)? {
            Some(c) => c,
            None => self
                .refs
                .resolve(&self.head_branch()?)?
                .ok_or("create an initial commit before 'alt flow init'")?,
        };
        let head_old = self.refs.get(&self.head_ref).cloned();
        self.refs.commit(
            &actor("flow"),
            now_ms(),
            &[
                RefChange {
                    name: dev_ref.clone(),
                    old: None,
                    new: Some(RefTarget::Oid(start)),
                },
                RefChange {
                    name: self.head_ref.clone(),
                    old: head_old,
                    new: Some(RefTarget::Symbolic(dev_ref.clone())),
                },
            ],
        )?;
        if json {
            crate::json::emit(
                out,
                vec![
                    ("develop", Json::str(model.develop)),
                    ("already_initialized", Json::Bool(false)),
                    ("commit", Json::str(start.to_string())),
                ],
            )?;
        } else {
            writeln!(
                out,
                "Initialized flow: created '{}' at {start}",
                model.develop
            )?;
        }
        Ok(())
    }

    /// `alt flow feature start <name>`: branch `feature/<name>` off `develop`
    /// and switch to it, atomically (one op log entry → O(1) undo).
    pub fn flow_feature_start(&mut self, name: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let flow = alt_flow::BranchModel::default().feature(name)?;
        let feat_ref = format!("refs/heads/{}", flow.branch);
        let base_ref = format!("refs/heads/{}", flow.base);
        if self.refs.get(&feat_ref).is_some() {
            return Err(format!("a branch named '{}' already exists", flow.branch).into());
        }
        let base = self
            .refs
            .resolve(&base_ref)?
            .ok_or("develop branch missing; run 'alt flow init' first")?;
        self.ensure_clean("flow start")?;

        // the feature branch starts at develop's commit, so its tree equals
        // develop's; materialize it (a no-op when already on develop)
        let target = self.commit_entries(base)?;
        let old = index_entries(&self.index()?);
        self.checkout(&old, &target)?;

        let head_old = self.refs.get(&self.head_ref).cloned();
        self.refs.commit(
            &actor("flow"),
            now_ms(),
            &[
                RefChange {
                    name: feat_ref.clone(),
                    old: None,
                    new: Some(RefTarget::Oid(base)),
                },
                RefChange {
                    name: self.head_ref.clone(),
                    old: head_old,
                    new: Some(RefTarget::Symbolic(feat_ref.clone())),
                },
            ],
        )?;
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("branch", Json::str(&flow.branch)),
                    ("base", Json::str(flow.base)),
                    ("commit", Json::str(base.to_string())),
                ],
            )?;
        } else {
            writeln!(out, "Switched to a new branch '{}'", flow.branch)?;
        }
        Ok(())
    }

    /// `alt flow feature finish <name>`: merge `feature/<name>` into `develop`,
    /// delete the feature branch, and move HEAD to `develop` — all in one
    /// atomic ref transaction (one op log entry → O(1) undo). Aborts if the
    /// merge conflicts.
    pub fn flow_feature_finish(&mut self, name: &str, json: bool, out: &mut impl Write) -> Res<()> {
        let flow = alt_flow::BranchModel::default().feature(name)?;
        let feat_ref = format!("refs/heads/{}", flow.branch);
        let dev_ref = format!("refs/heads/{}", flow.target);
        let feat = self
            .refs
            .resolve(&feat_ref)?
            .ok_or_else(|| format!("no such feature branch '{}'", flow.branch))?;
        let dev = self
            .refs
            .resolve(&dev_ref)?
            .ok_or("develop branch missing; run 'alt flow init' first")?;
        self.ensure_clean("flow finish")?;

        // merge the feature into develop without yet touching refs/work tree
        let (new_dev, entries) = match self.compute_merge(dev, feat, &flow.branch)? {
            MergeOutcome::UpToDate => (dev, self.commit_entries(dev)?),
            MergeOutcome::FastForward(c) => (c, self.commit_entries(c)?),
            MergeOutcome::Merged { commit, entries } => (commit, entries),
            MergeOutcome::Conflicted(_) => {
                return Err(format!(
                    "merge of '{}' into '{}' has conflicts; \
                     run 'alt merge' on '{}' and resolve manually",
                    flow.branch, flow.target, flow.target
                )
                .into());
            }
        };

        // one atomic op: advance develop, delete the feature, move HEAD
        let head_old = self.refs.get(&self.head_ref).cloned();
        self.refs.commit(
            &actor("flow"),
            now_ms(),
            &[
                RefChange {
                    name: dev_ref.clone(),
                    old: Some(RefTarget::Oid(dev)),
                    new: Some(RefTarget::Oid(new_dev)),
                },
                RefChange {
                    name: feat_ref.clone(),
                    old: Some(RefTarget::Oid(feat)),
                    new: None,
                },
                RefChange {
                    name: self.head_ref.clone(),
                    old: head_old,
                    new: Some(RefTarget::Symbolic(dev_ref.clone())),
                },
            ],
        )?;

        // bring the working tree to the merged develop
        let old = index_entries(&self.index()?);
        self.checkout(&old, &entries)?;
        if json {
            use crate::json::Json;
            crate::json::emit(
                out,
                vec![
                    ("target", Json::str(&flow.target)),
                    ("commit", Json::str(new_dev.to_string())),
                    ("deleted", Json::str(&flow.branch)),
                ],
            )?;
        } else {
            writeln!(
                out,
                "Merged '{}' into '{}' and deleted it",
                flow.branch, flow.target
            )?;
        }
        Ok(())
    }

    /// `alt undo`: invert the most recent ref transaction (restoring the prior
    /// branch/HEAD state) and re-materialize HEAD's tree. The inverse is
    /// itself recorded as an op, so undo is append-only and re-undoable.
    pub fn undo(&mut self, json: bool, out: &mut impl Write) -> Res<()> {
        let changes = self.refs.last_transaction()?.ok_or("nothing to undo")?;
        self.ensure_clean("undo")?;

        // capture the current HEAD tree to drive the checkout's removals
        let old = index_entries(&self.index()?);
        let inverse: Vec<RefChange> = changes
            .iter()
            .map(|c| RefChange {
                name: c.name.clone(),
                old: c.new.clone(),
                new: c.old.clone(),
            })
            .collect();
        self.refs.commit(&actor("undo"), now_ms(), &inverse)?;

        let target = self.head_entries()?;
        self.checkout(&old, &target)?;
        if json {
            use crate::json::Json;
            let refs = changes.iter().map(|c| Json::str(&c.name)).collect();
            crate::json::emit(
                out,
                vec![("undone", Json::Bool(true)), ("refs", Json::Array(refs))],
            )?;
        } else {
            writeln!(out, "Undid the last operation")?;
        }
        Ok(())
    }

    /// Advances `branch` from `old` to `new` in one ref transaction.
    fn advance_branch(&mut self, branch: &str, old: ObjectId, new: ObjectId) -> Res<()> {
        self.refs.commit(
            &actor("merge"),
            now_ms(),
            &[RefChange {
                name: branch.to_owned(),
                old: Some(RefTarget::Oid(old)),
                new: Some(RefTarget::Oid(new)),
            }],
        )?;
        Ok(())
    }

    /// The merge base (lowest common ancestor) of two commits, or `None` for
    /// unrelated histories. Picks one base when several exist (criss-cross
    /// histories aren't a dogfood concern yet).
    fn merge_base(&self, a: ObjectId, b: ObjectId) -> Res<Option<ObjectId>> {
        if a == b {
            return Ok(Some(a));
        }
        let anc_a = self.ancestors(a)?;
        let anc_b = self.ancestors(b)?;
        let common: std::collections::HashSet<ObjectId> =
            anc_a.intersection(&anc_b).copied().collect();
        // a base is a common commit that is not a proper ancestor of another
        // common commit (i.e. a maximal element of the common set)
        let mut bases = common.clone();
        for c in &common {
            for x in self.ancestors(*c)? {
                if x != *c {
                    bases.remove(&x);
                }
            }
        }
        Ok(bases.into_iter().next())
    }

    /// All commits reachable from `start` (inclusive) via parent links.
    fn ancestors(&self, start: ObjectId) -> Res<std::collections::HashSet<ObjectId>> {
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![start];
        while let Some(c) = stack.pop() {
            if !seen.insert(c) {
                continue;
            }
            let obj = self.odb.get(&c)?.ok_or("commit missing from store")?;
            for p in alt_git_codec::Commit::parse(&obj.data)?.parents() {
                if !seen.contains(&p) {
                    stack.push(p);
                }
            }
        }
        Ok(seen)
    }

    /// Three-way merges two trees over their common base, path by path.
    fn merge_trees(
        &mut self,
        base: &[WorkEntry],
        ours: &[WorkEntry],
        theirs: &[WorkEntry],
        their_label: &str,
    ) -> Res<Vec<Resolved>> {
        use std::collections::{BTreeSet, HashMap};
        let map = |es: &[WorkEntry]| -> HashMap<BString, WorkEntry> {
            es.iter().map(|e| (e.path.clone(), e.clone())).collect()
        };
        let (bm, om, tm) = (map(base), map(ours), map(theirs));
        let paths: BTreeSet<BString> = bm
            .keys()
            .chain(om.keys())
            .chain(tm.keys())
            .cloned()
            .collect();

        let mut out = Vec::with_capacity(paths.len());
        for path in paths {
            let bo = bm.get(&path).cloned();
            let ao = om.get(&path).cloned();
            let to = tm.get(&path).cloned();
            out.push(self.resolve_path(path, bo, ao, to, their_label)?);
        }
        Ok(out)
    }

    /// Resolves one path's three-way state into a clean entry or a conflict.
    fn resolve_path(
        &mut self,
        path: BString,
        bo: Option<WorkEntry>,
        ao: Option<WorkEntry>,
        to: Option<WorkEntry>,
        their_label: &str,
    ) -> Res<Resolved> {
        let same = |x: &Option<WorkEntry>, y: &Option<WorkEntry>| match (x, y) {
            (None, None) => true,
            (Some(p), Some(q)) => p.oid == q.oid && p.mode == q.mode,
            _ => false,
        };
        if same(&ao, &to) {
            return Ok(Resolved::clean(path, ao)); // both agree (incl. both-deleted)
        }
        if same(&ao, &bo) {
            return Ok(Resolved::clean(path, to)); // ours unchanged → take theirs
        }
        if same(&to, &bo) {
            return Ok(Resolved::clean(path, ao)); // theirs unchanged → take ours
        }

        // both sides diverged from base
        match (&ao, &to) {
            (Some(a), Some(t)) => {
                let base_bytes = match &bo {
                    Some(b) => self.blob_bytes(b.oid)?,
                    None => Vec::new(),
                };
                let ours_bytes = self.blob_bytes(a.oid)?;
                let theirs_bytes = self.blob_bytes(t.oid)?;
                let unmergeable = a.mode != t.mode
                    || alt_diff::is_binary(&base_bytes)
                    || alt_diff::is_binary(&ours_bytes)
                    || alt_diff::is_binary(&theirs_bytes);
                if unmergeable {
                    // keep ours in the working tree, record all three stages
                    return Ok(make_conflict(path, bo, ao, to, ours_bytes));
                }
                let labels = alt_merge::Labels {
                    ours: "HEAD",
                    theirs: their_label,
                };
                let m = alt_merge::merge(&base_bytes, &ours_bytes, &theirs_bytes, &labels);
                if m.is_clean() {
                    let oid = ObjectId::hash_object(self.algo, ObjectKind::Blob, &m.content);
                    self.odb.put(oid, ObjectKind::Blob, &m.content)?;
                    Ok(Resolved::clean(
                        path.clone(),
                        Some(WorkEntry {
                            path,
                            oid,
                            mode: a.mode,
                        }),
                    ))
                } else {
                    Ok(make_conflict(path, bo, ao, to, m.content))
                }
            }
            // modify/delete: one side changed the file, the other removed it
            (Some(a), None) => {
                let bytes = self.blob_bytes(a.oid)?;
                Ok(make_conflict(path, bo, ao, to, bytes))
            }
            (None, Some(t)) => {
                let bytes = self.blob_bytes(t.oid)?;
                Ok(make_conflict(path, bo, ao, to, bytes))
            }
            (None, None) => unreachable!("same(ao, to) already handled both-None"),
        }
    }

    /// Writes a conflicted merge state to disk: each resolution's working-tree
    /// bytes, then an index carrying stage-0 entries for clean paths and
    /// stage 1/2/3 entries for conflicted ones (so git tools see the merge).
    fn write_conflicted(&mut self, resolved: &[Resolved]) -> Res<()> {
        for r in resolved {
            let abs = self.abs(&r.path)?;
            if let Some(bytes) = &r.worktree {
                if let Some(parent) = abs.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if abs.symlink_metadata().is_ok() {
                    std::fs::remove_file(&abs)?;
                }
                std::fs::write(&abs, bytes)?;
            } else {
                match &r.entry {
                    Some(e) => self.materialize(e)?,
                    None => {
                        if abs.symlink_metadata().is_ok() {
                            std::fs::remove_file(&abs)?;
                            self.prune_empty_dirs(abs.parent());
                        }
                    }
                }
            }
        }

        let mut entries = Vec::new();
        for r in resolved {
            if r.conflicted {
                for (stage, w) in &r.stages {
                    entries.push(stage_entry(w, *stage));
                }
            } else if let Some(e) = &r.entry {
                entries.push(self.make_entry(e)?);
            }
        }
        save_index(
            &self.index_path,
            &Index {
                version: 2,
                entries,
                extensions: Vec::new(),
            },
        )?;
        Ok(())
    }
}

/// Renders `status` as the stable JSON schema (version 1):
/// `{schema_version, branch, staged:[{path,change}], unstaged:[...],
/// untracked:[path...], unmerged:[path...], clean:bool}`. `change` is one of
/// `added`/`modified`/`deleted`; the human view is a parallel rendering of the
/// same facts.
fn render_status_json(
    out: &mut impl Write,
    branch: &str,
    st: &alt_worktree::Status,
    unmerged: &std::collections::BTreeSet<BString>,
) -> Res<()> {
    use crate::json::Json;
    let change = |k: ChangeKind| match k {
        ChangeKind::Added => "added",
        ChangeKind::Modified => "modified",
        ChangeKind::Deleted => "deleted",
    };
    let entries = |v: &[(BString, ChangeKind)]| {
        Json::Array(
            v.iter()
                .map(|(p, k)| {
                    Json::Object(vec![
                        ("path", Json::str(p)),
                        ("change", Json::str(change(*k))),
                    ])
                })
                .collect(),
        )
    };
    let paths = |it: &mut dyn Iterator<Item = &BString>| Json::Array(it.map(Json::str).collect());
    let clean = unmerged.is_empty()
        && st.staged.is_empty()
        && st.unstaged.is_empty()
        && st.untracked.is_empty();
    let doc = Json::Object(vec![
        ("schema_version", Json::Num(1)),
        ("branch", Json::str(branch)),
        ("staged", entries(&st.staged)),
        ("unstaged", entries(&st.unstaged)),
        ("untracked", paths(&mut st.untracked.iter())),
        ("unmerged", paths(&mut unmerged.iter())),
        ("clean", Json::Bool(clean)),
    ]);
    doc.write(out)?;
    out.write_all(b"\n")?;
    Ok(())
}

/// One diff hunk as JSON: the `@@` coordinates plus tagged lines, where each
/// line is `{tag: context|add|remove, content}` (content keeps its raw bytes,
/// including any trailing newline).
fn hunk_json(h: &alt_diff::Hunk) -> crate::json::Json {
    use crate::json::Json;
    let lines = h
        .lines
        .iter()
        .map(|(tag, line)| {
            let kind = match tag {
                b'+' => "add",
                b'-' => "remove",
                _ => "context",
            };
            Json::Object(vec![("tag", Json::str(kind)), ("content", Json::str(line))])
        })
        .collect();
    Json::Object(vec![
        ("old_start", Json::Num(h.old_start as i64)),
        ("old_len", Json::Num(h.old_len as i64)),
        ("new_start", Json::Num(h.new_start as i64)),
        ("new_len", Json::Num(h.new_len as i64)),
        ("lines", Json::Array(lines)),
    ])
}

/// How a merge resolves once computed (no refs or work tree touched yet).
enum MergeOutcome {
    /// `theirs` is already an ancestor of `ours`; nothing to do.
    UpToDate,
    /// `ours` is an ancestor of `theirs`; advance to this commit.
    FastForward(ObjectId),
    /// A clean three-way merge: this merge commit and its flattened tree.
    Merged {
        commit: ObjectId,
        entries: Vec<WorkEntry>,
    },
    /// At least one path conflicts; the per-path resolutions to write out.
    Conflicted(Vec<Resolved>),
}

/// One path's merge resolution.
struct Resolved {
    path: BString,
    /// The clean result entry (`None` = deleted); meaningless when conflicted.
    entry: Option<WorkEntry>,
    conflicted: bool,
    /// Bytes to write to the working tree on conflict (markers or the kept
    /// side); `None` for a clean resolution.
    worktree: Option<Vec<u8>>,
    /// Unmerged index entries `(stage, entry)` for a conflict.
    stages: Vec<(u8, WorkEntry)>,
}

impl Resolved {
    fn clean(path: BString, entry: Option<WorkEntry>) -> Self {
        Resolved {
            path,
            entry,
            conflicted: false,
            worktree: None,
            stages: Vec::new(),
        }
    }
}

/// Builds a conflicted resolution: working-tree `bytes` plus stage 1/2/3
/// index entries for whichever of base/ours/theirs are present.
fn make_conflict(
    path: BString,
    bo: Option<WorkEntry>,
    ao: Option<WorkEntry>,
    to: Option<WorkEntry>,
    bytes: Vec<u8>,
) -> Resolved {
    let mut stages = Vec::new();
    if let Some(b) = bo {
        stages.push((1u8, b));
    }
    if let Some(a) = ao {
        stages.push((2u8, a));
    }
    if let Some(t) = to {
        stages.push((3u8, t));
    }
    Resolved {
        path,
        entry: None,
        conflicted: true,
        worktree: Some(bytes),
        stages,
    }
}

/// A zero-stat index entry at a given merge `stage` (1=base, 2=ours,
/// 3=theirs).
fn stage_entry(w: &WorkEntry, stage: u8) -> IndexEntry {
    IndexEntry {
        ctime: (0, 0),
        mtime: (0, 0),
        dev: 0,
        ino: 0,
        mode: w.mode,
        uid: 0,
        gid: 0,
        size: 0,
        oid: w.oid,
        flags: ((stage as u16) << 12) | (w.path.len().min(0x0FFF) as u16),
        extended_flags: None,
        path: w.path.clone(),
    }
}

#[cfg(unix)]
fn symlink_to(target: &[u8], at: &Path) -> Res<()> {
    use std::os::unix::ffi::OsStrExt;
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), at)?;
    Ok(())
}

#[cfg(not(unix))]
fn symlink_to(target: &[u8], at: &Path) -> Res<()> {
    // no symlink support: fall back to a regular file holding the target text
    std::fs::write(at, target)?;
    Ok(())
}

#[cfg(unix)]
fn set_exec(path: &Path, exec: bool) -> Res<()> {
    use std::os::unix::fs::PermissionsExt;
    let perm = if exec { 0o755 } else { 0o644 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(perm))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_exec(_path: &Path, _exec: bool) -> Res<()> {
    Ok(())
}

/// Minimal git check-ref-format: no empty/space/control chars, no `..`,
/// no leading/trailing slash, no segment starting with a dot.
fn check_branch_name(name: &str) -> Res<()> {
    let bad = name.is_empty()
        || name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("//")
        || name.contains(['\\', ' ', '~', '^', ':', '?', '*', '['])
        || name.bytes().any(|b| b < 0x20 || b == 0x7f)
        || name
            .split('/')
            .any(|seg| seg.is_empty() || seg.starts_with('.'));
    if bad {
        return Err(format!("'{name}' is not a valid branch name").into());
    }
    Ok(())
}

/// Reads a working tree's `.alt` marker file: line 1 is the repo root (the
/// directory holding the real `.alt`), line 2 is the workspace name.
fn parse_workspace_marker(path: &Path) -> Res<(PathBuf, String)> {
    let content = std::fs::read_to_string(path)?;
    let mut lines = content.lines();
    let repo_root = lines.next().ok_or("malformed .alt workspace marker")?;
    let name = lines.next().ok_or("malformed .alt workspace marker")?;
    Ok((PathBuf::from(repo_root), name.to_owned()))
}

/// A workspace name: a single path segment (it becomes part of the ref name
/// `workspaces/<name>/HEAD` and a directory), so no slashes, dots, control or
/// special chars, and not the reserved default name.
fn check_workspace_name(name: &str) -> Res<()> {
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

/// A 7-hex-char object id abbreviation, or all-zero for an absent side.
fn abbrev(oid: Option<ObjectId>) -> String {
    match oid {
        Some(o) => o.to_string()[..7].to_owned(),
        None => "0000000".to_owned(),
    }
}

fn identity() -> (String, String) {
    let name = std::env::var("GIT_AUTHOR_NAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "alt".to_owned());
    let email = std::env::var("GIT_AUTHOR_EMAIL").unwrap_or_else(|_| format!("{name}@localhost"));
    (name, email)
}

fn empty_index() -> Index {
    Index {
        version: 2,
        entries: Vec::new(),
        extensions: Vec::new(),
    }
}

/// Atomic index write to `path`: temp file + rename (sibling temp).
fn save_index(path: &Path, index: &Index) -> Res<()> {
    let bytes = index.serialize(HashAlgo::Sha1);
    let tmp = path.with_extension("tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Builds an index entry for a working-tree file, filling stat from its
/// metadata so git reads our index without spurious "modified".
#[cfg(unix)]
fn stat_entry(meta: &std::fs::Metadata, w: &WorkEntry) -> IndexEntry {
    use std::os::unix::fs::MetadataExt;
    IndexEntry {
        ctime: (meta.ctime() as u32, meta.ctime_nsec() as u32),
        mtime: (meta.mtime() as u32, meta.mtime_nsec() as u32),
        dev: meta.dev() as u32,
        ino: meta.ino() as u32,
        mode: w.mode,
        uid: meta.uid(),
        gid: meta.gid(),
        size: meta.size() as u32,
        oid: w.oid,
        flags: (w.path.len().min(0x0FFF)) as u16,
        extended_flags: None,
        path: w.path.clone(),
    }
}

#[cfg(not(unix))]
fn stat_entry(_meta: &std::fs::Metadata, w: &WorkEntry) -> IndexEntry {
    IndexEntry {
        ctime: (0, 0),
        mtime: (0, 0),
        dev: 0,
        ino: 0,
        mode: w.mode,
        uid: 0,
        gid: 0,
        size: 0,
        oid: w.oid,
        flags: (w.path.len().min(0x0FFF)) as u16,
        extended_flags: None,
        path: w.path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A named workspace gets its own HEAD, index, and working tree, while the
    /// odb and branch refs stay shared — so work in one does not disturb the
    /// other.
    #[test]
    fn workspaces_isolate_head_index_and_working_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut sink = Vec::new();

        // default workspace: first commit on main, then a `feat` branch
        init(Some(root.to_path_buf()), &mut sink).unwrap();
        std::fs::write(root.join("a.txt"), "main\n").unwrap();
        let mut repo = NativeRepo::discover(root).unwrap();
        repo.add(&[".".to_owned()], false, &mut sink).unwrap();
        repo.commit("first", false, &mut sink).unwrap();
        repo.branch(Some("feat".to_owned()), None, false, &mut sink)
            .unwrap();

        // a second workspace on `feat`, in its own working tree outside the repo
        let wt_tmp = tempfile::tempdir().unwrap();
        let wt = wt_tmp.path().join("ws2-tree");
        repo.create_workspace("ws2", &wt, "feat").unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.join("a.txt")).unwrap(),
            "main\n",
            "new workspace materializes its branch tree"
        );

        // commit in ws2 (advancing feat) — the default workspace is untouched
        std::fs::write(wt.join("a.txt"), "feat-work\n").unwrap();
        let mut ws2 = NativeRepo::open_workspace(root, "ws2").unwrap();
        ws2.add(&[".".to_owned()], false, &mut sink).unwrap();
        ws2.commit("ws2 work", false, &mut sink).unwrap();

        // default workspace: still on main, working tree and HEAD unchanged
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "main\n"
        );
        let def = NativeRepo::discover(root).unwrap();
        assert_eq!(def.head_branch().unwrap(), "refs/heads/main");
        assert_eq!(
            NativeRepo::open_workspace(root, "ws2")
                .unwrap()
                .head_branch()
                .unwrap(),
            "refs/heads/feat",
            "ws2 is on feat"
        );

        // shared store: the default workspace sees feat advanced by ws2
        assert!(def.refs.resolve("refs/heads/feat").unwrap().is_some());

        // listing shows both; removing ws2 drops it
        let names: Vec<String> = def
            .list_workspaces()
            .unwrap()
            .into_iter()
            .map(|(n, ..)| n)
            .collect();
        assert!(names.contains(&"default".to_owned()));
        assert!(names.contains(&"ws2".to_owned()));
        NativeRepo::discover(root)
            .unwrap()
            .remove_workspace("ws2")
            .unwrap();
        assert!(NativeRepo::open_workspace(root, "ws2").is_err());
    }

    #[test]
    fn the_default_workspace_cannot_be_removed_and_bad_names_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut sink = Vec::new();
        init(Some(root.to_path_buf()), &mut sink).unwrap();
        std::fs::write(root.join("a.txt"), "x\n").unwrap();
        let mut repo = NativeRepo::discover(root).unwrap();
        repo.add(&[".".to_owned()], false, &mut sink).unwrap();
        repo.commit("c", false, &mut sink).unwrap();

        assert!(repo.remove_workspace("default").is_err());
        let wt = root.join("bad");
        assert!(repo.create_workspace("with/slash", &wt, "main").is_err());
        assert!(repo.create_workspace("default", &wt, "main").is_err());
        assert!(repo.create_workspace("ok", &wt, "no-such-branch").is_err());
    }
}
