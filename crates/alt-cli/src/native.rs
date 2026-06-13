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
    save_index(&alt_dir, &empty_index())?;
    writeln!(
        out,
        "Initialized empty alt repository in {}",
        alt_dir.display()
    )?;
    Ok(())
}

/// An opened native repo.
pub struct NativeRepo {
    root: PathBuf,
    alt_dir: PathBuf,
    odb: NativeOdb,
    refs: RefStore,
    algo: HashAlgo,
}

impl NativeRepo {
    /// Walks up from `start` for a directory holding `.alt`.
    pub fn discover(start: &Path) -> Res<Self> {
        let mut dir: &Path = start;
        loop {
            if dir.join(".alt").is_dir() {
                let alt_dir = dir.join(".alt");
                return Ok(Self {
                    root: dir.to_path_buf(),
                    odb: NativeOdb::open(&alt_dir)?,
                    refs: RefStore::open(&alt_dir)?,
                    alt_dir,
                    algo: HashAlgo::Sha1,
                });
            }
            dir = dir
                .parent()
                .ok_or("not an alt repository (no .alt found)")?;
        }
    }

    fn index(&self) -> Res<Index> {
        match Index::open(&self.alt_dir.join("index"), self.algo) {
            Ok(i) => Ok(i),
            Err(alt_git_index::IndexError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(empty_index())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// `alt add <paths>`: stage the given paths (or everything for `.`),
    /// updating the index to match the working tree.
    pub fn add(&mut self, paths: &[String], out: &mut impl Write) -> Res<()> {
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
            &self.alt_dir,
            &Index {
                version: 2,
                entries,
                extensions: Vec::new(),
            },
        )?;
        writeln!(out, "staged {staged} path(s)")?;
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
    pub fn commit(&mut self, message: &str, out: &mut impl Write) -> Res<()> {
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
        writeln!(
            out,
            "[{}] {commit}",
            branch.strip_prefix("refs/heads/").unwrap_or(&branch)
        )?;
        Ok(())
    }

    /// `alt status`: staged / unstaged / untracked against HEAD and the index.
    pub fn status(&self, out: &mut impl Write) -> Res<()> {
        let branch = self.head_branch()?;
        let head_tree = match self.refs.resolve(&branch)? {
            Some(commit) => {
                let obj = self
                    .odb
                    .get(&commit)?
                    .ok_or("HEAD commit missing from store")?;
                Some(
                    alt_git_codec::Commit::parse(&obj.data)?
                        .tree()
                        .ok_or("commit has no tree")?,
                )
            }
            None => None,
        };
        let head = match head_tree {
            Some(t) => flatten_tree(&self.odb, t, self.algo)?,
            None => Vec::new(),
        };
        let index = index_entries(&self.index()?);
        let worktree = scan_worktree(&self.root, self.algo)?;
        let st = status(&head, &index, &worktree);

        let short = branch.strip_prefix("refs/heads/").unwrap_or(&branch);
        writeln!(out, "On branch {short}")?;
        let mark = |k: ChangeKind| match k {
            ChangeKind::Added => "new file",
            ChangeKind::Modified => "modified",
            ChangeKind::Deleted => "deleted",
        };
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
        if st.staged.is_empty() && st.unstaged.is_empty() && st.untracked.is_empty() {
            writeln!(out, "nothing to commit, working tree clean")?;
        }
        Ok(())
    }

    fn head_branch(&self) -> Res<String> {
        match self.refs.get("HEAD") {
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
        out: &mut impl Write,
    ) -> Res<()> {
        match (name, delete) {
            (_, Some(target)) => self.delete_branch(&target, out),
            (Some(new), None) => self.create_branch(&new, out),
            (None, None) => self.list_branches(out),
        }
    }

    fn list_branches(&self, out: &mut impl Write) -> Res<()> {
        let current = self.head_branch()?;
        for (name, _) in self.refs.iter() {
            if let Some(short) = name.strip_prefix("refs/heads/") {
                let mark = if name == current { "* " } else { "  " };
                writeln!(out, "{mark}{short}")?;
            }
        }
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
    pub fn switch(&mut self, name: &str, create: bool, out: &mut impl Write) -> Res<()> {
        let full = format!("refs/heads/{name}");
        let current = self.head_branch()?;

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
            writeln!(out, "Switched to a new branch '{name}'")?;
            return Ok(());
        }

        if self.refs.get(&full).is_none() {
            return Err(format!("invalid reference: {name}").into());
        }
        if full == current {
            writeln!(out, "Already on '{name}'")?;
            return Ok(());
        }
        self.ensure_clean_for_switch()?;

        let target = match self.refs.resolve(&full)? {
            Some(commit) => {
                let obj = self.odb.get(&commit)?.ok_or("branch commit missing")?;
                let tree = alt_git_codec::Commit::parse(&obj.data)?
                    .tree()
                    .ok_or("commit has no tree")?;
                flatten_tree(&self.odb, tree, self.algo)?
            }
            None => Vec::new(), // unborn target: tree becomes empty
        };
        let old = index_entries(&self.index()?);
        self.checkout(&old, &target)?;
        self.move_head(&current, &full)?;
        writeln!(out, "Switched to branch '{name}'")?;
        Ok(())
    }

    /// Refuses the switch if any tracked file has staged or unstaged changes.
    fn ensure_clean_for_switch(&self) -> Res<()> {
        let branch = self.head_branch()?;
        let head = match self.refs.resolve(&branch)? {
            Some(commit) => {
                let obj = self.odb.get(&commit)?.ok_or("HEAD commit missing")?;
                let tree = alt_git_codec::Commit::parse(&obj.data)?
                    .tree()
                    .ok_or("commit has no tree")?;
                flatten_tree(&self.odb, tree, self.algo)?
            }
            None => Vec::new(),
        };
        let index = index_entries(&self.index()?);
        let worktree = scan_worktree(&self.root, self.algo)?;
        let st = status(&head, &index, &worktree);
        if !st.staged.is_empty() || !st.unstaged.is_empty() {
            return Err("your local changes would be overwritten by switch; \
                 commit them first"
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
            &self.alt_dir,
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
        let old = self.refs.get("HEAD").cloned();
        self.refs.commit(
            &actor("switch"),
            now_ms(),
            &[RefChange {
                name: "HEAD".to_owned(),
                old: old.or_else(|| Some(RefTarget::Symbolic(from.to_owned()))),
                new: Some(RefTarget::Symbolic(to.to_owned())),
            }],
        )?;
        Ok(())
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

/// Atomic index write: temp file + rename.
fn save_index(alt_dir: &Path, index: &Index) -> Res<()> {
    let bytes = index.serialize(HashAlgo::Sha1);
    let path = alt_dir.join("index");
    let tmp = alt_dir.join("index.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
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
