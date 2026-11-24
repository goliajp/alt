//! Where in a repository a command works: its workspace.

use super::workspace::parse_workspace_marker;
use super::*;

mod owned;

pub use owned::discover_owned;

/// The per-workspace coordinates within a repo: the working-tree root, the
/// workspace name, the ref naming its HEAD, and its index file. Resolved per
/// request and cheap to carry by value.
#[derive(Clone)]
pub struct Coord {
    pub(super) root: PathBuf,
    pub(super) workspace: String,
    pub(super) head_ref: String,
    pub(super) index_path: PathBuf,
    /// where beneath `root` the command was run: `sub/dir`, empty at the
    /// root and for a workspace named from outside its tree
    pub(super) prefix: String,
}

impl Coord {
    /// The ref this workspace's HEAD lives in: `HEAD` for the default
    /// workspace, `workspaces/<name>/HEAD` for a named one.
    pub fn head_ref(&self) -> &str {
        &self.head_ref
    }

    /// The workspace's working-tree root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The default workspace's coordinates for the repo rooted at `repo_root`
    /// (its HEAD stays the bare `HEAD` ref and its index `.alt/index`, so
    /// existing repos are unchanged).
    pub(super) fn default_at(repo_root: &Path) -> Coord {
        let alt_dir = repo_root.join(".alt");
        Coord {
            index_path: alt_dir.join("index"),
            root: repo_root.to_path_buf(),
            workspace: DEFAULT_WORKSPACE.to_owned(),
            head_ref: "HEAD".to_owned(),
            prefix: String::new(),
        }
    }

    /// The coordinates of workspace `name` in the repo whose control dir is
    /// `alt_dir`. The default workspace's working tree is the repo root; a
    /// named one's comes from its registry `meta`.
    pub(super) fn for_name(alt_dir: &Path, name: &str) -> Res<Coord> {
        let repo_root = alt_dir.parent().unwrap_or(alt_dir);
        if name == DEFAULT_WORKSPACE {
            return Ok(Coord::default_at(repo_root));
        }
        let ws_dir = alt_dir.join("workspaces").join(name);
        let worktree = std::fs::read_to_string(ws_dir.join("meta"))
            .map_err(|_| format!("no such workspace '{name}'"))?;
        Ok(Coord {
            root: PathBuf::from(worktree.trim()),
            workspace: name.to_owned(),
            head_ref: format!("workspaces/{name}/HEAD"),
            index_path: ws_dir.join("index"),
            prefix: String::new(),
        })
    }

    /// These coordinates for a command run in `start`, which lies in the
    /// working tree `tree` of the workspace `found`: the paths the command
    /// names are relative to `start` where that is the tree it works on.
    fn run_from(mut self, tree: &Path, start: &Path, found: &str) -> Coord {
        if self.workspace == found {
            self.prefix = pathspec::prefix_of(tree, start);
        }
        self
    }
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
            owned::ensure_owned(dir, &[dir, &marker])?;
            let coord = Coord::for_name(&marker, workspace.unwrap_or(DEFAULT_WORKSPACE))?;
            return Ok((marker, coord.run_from(dir, start, DEFAULT_WORKSPACE)));
        }
        if marker.is_file() {
            let (repo_root, name) = parse_workspace_marker(&marker)?;
            let alt_dir = repo_root.join(".alt");
            owned::ensure_owned(dir, &[dir, &marker, &alt_dir])?;
            let coord = Coord::for_name(&alt_dir, workspace.unwrap_or(&name))?;
            return Ok((alt_dir, coord.run_from(dir, start, &name)));
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

/// The branch checked out in the workspace a command run in `cwd` works
/// in, read from the refs as they are and without opening the store;
/// `None` where that workspace is on no branch or cannot be read.
pub fn branch_at(cwd: &Path, workspace: Option<&str>) -> Option<BString> {
    let (alt_dir, coord) = resolve_workspace(cwd, workspace).ok()?;
    let refs = RefStore::open_read_only(&alt_dir).ok()?;
    let full = match refs.get(&coord.head_ref) {
        Some(RefTarget::Symbolic(branch)) => branch.as_str(),
        Some(RefTarget::Oid(_)) => return None,
        None => "refs/heads/main",
    };
    Some(BString::from(full.strip_prefix("refs/heads/")?))
}
