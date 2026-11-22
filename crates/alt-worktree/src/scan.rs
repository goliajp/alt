//! Reading the whole working tree into entries: every file hashed, or taken
//! from an index whose entries carry stat. `status`, `diff` and `add` use
//! the snapshot-driven scan instead.

use std::path::Path;

use alt_bytes::{BString, ByteSlice};
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_git_index::Index;

use crate::{WorkEntry, WorktreeError, ignore};

/// Reads every tracked file in the working tree under `root` (skipping the
/// `.alt`/`.git` control dirs), hashing each to its blob id. Symlinks are
/// hashed by their target text (mode 120000), executables get mode 100755.
/// Returns entries sorted by path. **For status / diff on a real
/// monorepo, prefer [`scan_worktree_with_index`]** — it skips the read+hash
/// when the file's stat matches the index entry (git's classic stat
/// cache), turning a 50k-file scan from seconds into milliseconds when
/// nothing changed.
pub fn scan_worktree(root: &Path, algo: HashAlgo) -> Result<Vec<WorkEntry>, WorktreeError> {
    let mut out = Vec::new();
    let user = ignore::default_excludes_file();
    let mut stack = ignore::IgnoreStack::of_files(user.as_deref().as_slice())?;
    let tracked = Tracked::default();
    let ctx = ScanCtx {
        root,
        algo,
        cache: None,
        tracked: &tracked,
        clean: None,
        head: None,
    };
    scan_dir(&ctx, root, false, &mut stack, &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Same as [`scan_worktree`] but consults `index` to short-circuit the
/// read+hash on files whose `(mtime, ctime, size, dev, ino, mode)`
/// reproduce the recorded stat — the matching index entry's oid + mode
/// are reused verbatim. A status sweep over an unchanged monorepo then
/// touches each file's inode once and reads nothing. A mismatch (the file
/// was edited) falls through to the read+hash, so a stale stat can never
/// report wrong content. `excludes` are the ignore files outside the
/// working tree, a later one above an earlier.
pub fn scan_worktree_with_index(
    root: &Path,
    index: &Index,
    algo: HashAlgo,
    clean: Option<Clean<'_>>,
    head: Option<SubmoduleHead<'_>>,
    excludes: &[&Path],
) -> Result<Vec<WorkEntry>, WorktreeError> {
    let mut by_path: std::collections::HashMap<&BString, &alt_git_index::IndexEntry> =
        std::collections::HashMap::with_capacity(index.entries.len());
    for e in &index.entries {
        if e.stage() == 0 {
            by_path.insert(&e.path, e);
        }
    }
    let mut out = Vec::new();
    let mut stack = ignore::IgnoreStack::of_files(excludes)?;
    let tracked = Tracked::of(index);
    let ctx = ScanCtx {
        root,
        algo,
        cache: Some(&by_path),
        tracked: &tracked,
        clean,
        head,
    };
    scan_dir(&ctx, root, false, &mut stack, &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

type StatCache<'a> = std::collections::HashMap<&'a BString, &'a alt_git_index::IndexEntry>;

/// The index's paths and every directory above them. Ignore rules only
/// hide untracked files, as in git: a tracked file stays in the scan even
/// when a pattern matches it or a directory above it.
#[derive(Default)]
struct Tracked {
    files: std::collections::HashSet<BString>,
    dirs: std::collections::HashSet<BString>,
    /// the paths the index records as submodules
    gitlinks: std::collections::HashSet<BString>,
}

impl Tracked {
    fn of(index: &Index) -> Tracked {
        let mut t = Tracked::default();
        for e in &index.entries {
            let mut p = e.path.as_slice();
            while let Some(i) = p.rfind_byte(b'/') {
                p = &p[..i];
                if !t.dirs.insert(BString::from(p)) {
                    break;
                }
            }
            t.files.insert(e.path.clone());
            if e.mode == 0o160000 && e.stage() == 0 {
                t.gitlinks.insert(e.path.clone());
            }
        }
        t
    }
}

/// What stays the same for a whole scan.
struct ScanCtx<'a> {
    root: &'a Path,
    algo: HashAlgo,
    cache: Option<&'a StatCache<'a>>,
    tracked: &'a Tracked,
    clean: Option<Clean<'a>>,
    head: Option<SubmoduleHead<'a>>,
}

/// What a submodule directory has checked out: the commit its HEAD names,
/// or `None` when the directory holds no repository.
pub type SubmoduleHead<'a> = &'a dyn Fn(&Path) -> Option<ObjectId>;

/// A clean filter, as git's filter drivers have: given a path and the
/// working-tree bytes, the bytes to store instead, or `None` to store the
/// file as is. LFS uses it to stand a pointer in for a large file.
pub type Clean<'a> = &'a dyn Fn(&[u8], &[u8]) -> Option<Vec<u8>>;

fn cleaned(clean: Option<Clean<'_>>, path: &[u8], content: Vec<u8>) -> Vec<u8> {
    clean.and_then(|f| f(path, &content)).unwrap_or(content)
}

/// Matches paths against gitignore-style patterns (as `.gitattributes`
/// lines use), anchored at the working-tree root.
pub struct PathMatcher(ignore::IgnoreStack);

impl PathMatcher {
    /// One pattern per entry, gitignore syntax.
    pub fn new<'a>(patterns: impl IntoIterator<Item = &'a [u8]>) -> PathMatcher {
        let lines: Vec<&[u8]> = patterns.into_iter().collect();
        let mut stack = ignore::IgnoreStack::new();
        stack.push(ignore::parse_layer(&lines.join(&b'\n'), b""));
        PathMatcher(stack)
    }

    pub fn matches(&self, path: &[u8]) -> bool {
        self.0.matches_within(path)
    }
}

/// Scans `dir` into `out`. Inside an ignored directory (`only_tracked`) just
/// the tracked files are picked up.
fn scan_dir(
    ctx: &ScanCtx<'_>,
    dir: &Path,
    only_tracked: bool,
    ignore_stack: &mut ignore::IgnoreStack,
    out: &mut Vec<WorkEntry>,
) -> Result<(), WorktreeError> {
    let (root, algo, cache, tracked) = (ctx.root, ctx.algo, ctx.cache, ctx.tracked);
    // Load `<dir>/.gitignore` if present and push a layer for the duration
    // of this directory. Layer base is `dir`'s path relative to the root.
    let pushed = match std::fs::read(dir.join(".gitignore")) {
        Ok(bytes) => {
            let base = rel_path(dir.strip_prefix(root).unwrap());
            ignore_stack.push(ignore::parse_layer(&bytes, base.as_slice()));
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.into()),
    };

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".alt" || name == ".git" {
            continue; // control directories are not working-tree content
        }
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        let rel = path.strip_prefix(root).unwrap();
        let rel_b = rel_path(rel);
        let is_dir = meta.is_dir();
        let hidden = only_tracked || ignore_stack.is_ignored(rel_b.as_slice(), is_dir);
        let known = if is_dir {
            &tracked.dirs
        } else {
            &tracked.files
        };
        if hidden && !known.contains(&rel_b) {
            continue; // untracked and masked by an ignore rule in scope
        }
        if is_dir {
            // A submodule directory carries its own git layout (a `.git`
            // file pointing at the gitdir, or a real `.git` dir). It's a
            // separate repo, not working-tree content of this one — don't
            // recurse, and don't synthesize a worktree entry for it: the
            // submodule's HEAD oid is owned by the submodule, not us.
            // The same holds for a directory the index records as a
            // submodule, with or without a repository in it: whatever lies
            // there is not this repository's to add. What it reports is
            // the commit the submodule has checked out, or the recorded one
            if tracked.gitlinks.contains(&rel_b) {
                let recorded = cache.and_then(|c| c.get(&rel_b)).map(|e| e.oid);
                if let Some(oid) = ctx.head.and_then(|head| head(&path)).or(recorded) {
                    out.push(WorkEntry {
                        path: rel_b,
                        oid,
                        mode: 0o160000,
                    });
                }
                continue;
            }
            if is_submodule_dir(&path) {
                continue;
            }
            scan_dir(ctx, &path, hidden, ignore_stack, out)?;
            continue;
        }
        let mode = if meta.is_symlink() {
            0o120000
        } else {
            file_mode(&meta)
        };

        // Fast path: index has this path, and the recorded stat matches
        // what the kernel just told us — trust the recorded oid + mode
        // and don't read the file.
        if let Some(map) = cache
            && let Some(idx) = map.get(&rel_b)
            && stat_matches(idx, &meta, mode)
        {
            out.push(WorkEntry {
                path: rel_b,
                oid: idx.oid,
                mode: idx.mode,
            });
            continue;
        }

        let content = if meta.is_symlink() {
            std::fs::read_link(&path)?
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            cleaned(ctx.clean, &rel_b, std::fs::read(&path)?)
        };
        out.push(WorkEntry {
            path: rel_b,
            oid: ObjectId::hash_object(algo, ObjectKind::Blob, &content),
            mode,
        });
    }

    if pushed {
        ignore_stack.pop();
    }
    Ok(())
}

/// Git's stat-cache check (a tightened form): the working-tree file
/// reproduces the index entry's mode, size, mtime, ctime, dev, ino. Any
/// mismatch demotes the entry to the read+hash slow path.
#[cfg(unix)]
fn stat_matches(idx: &alt_git_index::IndexEntry, meta: &std::fs::Metadata, wt_mode: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    if idx.mode != wt_mode {
        return false;
    }
    if idx.size as u64 != meta.size() {
        return false;
    }
    // mtime / ctime: index stores sec + nsec; meta gives both
    if idx.mtime.0 as i64 != meta.mtime() || idx.mtime.1 as i64 != meta.mtime_nsec() {
        return false;
    }
    if idx.ctime.0 as i64 != meta.ctime() || idx.ctime.1 as i64 != meta.ctime_nsec() {
        return false;
    }
    if idx.dev != meta.dev() as u32 || idx.ino != meta.ino() as u32 {
        return false;
    }
    true
}

#[cfg(not(unix))]
fn stat_matches(idx: &alt_git_index::IndexEntry, meta: &std::fs::Metadata, wt_mode: u32) -> bool {
    // No fine-grained stat on non-unix; fall through to the read+hash slow
    // path. The same restriction applies to git on non-unix.
    let _ = (idx, meta, wt_mode);
    false
}

/// True when `dir` looks like a submodule worktree: it holds a `.git` entry
/// (either a directory, for a freestanding clone, or a gitfile pointing at
/// the parent's `.git/modules/...`).
fn is_submodule_dir(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(".git")).is_ok()
}

#[cfg(unix)]
fn file_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    if meta.mode() & 0o111 != 0 {
        0o100755
    } else {
        0o100644
    }
}

#[cfg(not(unix))]
fn file_mode(_meta: &std::fs::Metadata) -> u32 {
    0o100644
}

/// A relative filesystem path as a git path (forward slashes).
fn rel_path(rel: &Path) -> BString {
    let mut s = Vec::new();
    for (i, comp) in rel.components().enumerate() {
        if i > 0 {
            s.push(b'/');
        }
        s.extend_from_slice(comp.as_os_str().as_encoded_bytes());
    }
    BString::from(s)
}
