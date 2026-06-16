//! Working-tree status for a native `.alt` repo: scan the working tree,
//! flatten a stored tree, and three-way compare HEAD / index / worktree to
//! produce staged, unstaged, and untracked changes — the data behind
//! `alt status` and the basis `alt add` / `alt commit` build on.
//!
//! Steel: domain-aware (it knows git object kinds and the index) but bound
//! to no specific command.

use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind, Tree};
use alt_git_index::Index;
use alt_odb::{NativeOdb, OdbError};
use bstr::{BString, ByteSlice};

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("io")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Odb(#[from] OdbError),
    #[error(transparent)]
    Object(#[from] alt_git_codec::ObjectParseError),
    #[error("object {0} is not a tree")]
    NotATree(ObjectId),
}

/// One path's identity: its git blob/link id and mode. The unit the three
/// views (HEAD, index, worktree) are compared in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkEntry {
    pub path: BString,
    pub oid: ObjectId,
    pub mode: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

/// What `alt status` reports: index vs HEAD (staged), worktree vs index
/// (unstaged), and worktree files absent from the index (untracked).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub staged: Vec<(BString, ChangeKind)>,
    pub unstaged: Vec<(BString, ChangeKind)>,
    pub untracked: Vec<BString>,
}

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
    scan_dir(root, root, algo, None, &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Same as [`scan_worktree`] but consults `index` to short-circuit the
/// read+hash on files whose `(mtime, ctime, size, dev, ino, mode)`
/// reproduce the recorded stat — the matching index entry's oid + mode
/// are reused verbatim. A status sweep over an unchanged monorepo then
/// touches each file's inode once (the `symlink_metadata` syscall) and
/// reads nothing.
///
/// The fast path is byte-identical to the slow path when the stat
/// matches: the index stores the oid the working tree produced last time,
/// so reusing it preserves the structural-fidelity invariant. A mismatch
/// (the file was edited) falls through to the read+hash, so a stale stat
/// can never report wrong content.
pub fn scan_worktree_with_index(
    root: &Path,
    index: &Index,
    algo: HashAlgo,
) -> Result<Vec<WorkEntry>, WorktreeError> {
    let mut by_path: std::collections::HashMap<&BString, &alt_git_index::IndexEntry> =
        std::collections::HashMap::with_capacity(index.entries.len());
    for e in &index.entries {
        if e.stage() == 0 {
            by_path.insert(&e.path, e);
        }
    }
    let mut out = Vec::new();
    scan_dir(root, root, algo, Some(&by_path), &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

type StatCache<'a> = std::collections::HashMap<&'a BString, &'a alt_git_index::IndexEntry>;

/// Sparse worktree view driven by the index alone (no directory walk).
/// One stat per indexed path; matches the index → reuse `(oid, mode)`,
/// stat differs → read+hash, file missing → skip the entry so a `diff`
/// caller naturally sees "deleted" against the old (index) side. Untracked
/// files are invisible here — exactly what `alt diff` (uncached) wants;
/// `status` keeps `scan_worktree_with_index` so it can still report
/// untracked.
///
/// Wins over the full walk: `git diff` shape rather than `git status`
/// shape — O(index) stat syscalls instead of O(working-tree) read_dir +
/// stat. On a 50k-file monorepo with nothing changed this drops `alt diff`
/// from seconds (full walk + stat each) to ≤100 ms (one stat per index
/// entry, no opendir traffic).
pub fn scan_indexed_paths(
    root: &Path,
    index: &Index,
    algo: HashAlgo,
) -> Result<Vec<WorkEntry>, WorktreeError> {
    let mut out = Vec::with_capacity(index.entries.len());
    for idx in &index.entries {
        if idx.stage() != 0 {
            continue; // unmerged stages are reported via the merge UI
        }
        let rel = idx.path.to_path().map_err(|_| {
            WorktreeError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "non-utf8 path",
            ))
        })?;
        let abs = root.join(rel);
        let meta = match std::fs::symlink_metadata(&abs) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // file deleted from working tree → don't push; changes()
                // detects the asymmetry against the old (index) list and
                // emits a Deleted change
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let wt_mode = if meta.is_symlink() {
            0o120000
        } else if meta.is_dir() {
            // an indexed file path is now a directory — surface as a
            // delete + an untracked (we don't emit untracked here, so the
            // diff just sees a delete; status() catches the rest)
            continue;
        } else {
            file_mode(&meta)
        };
        if stat_matches(idx, &meta, wt_mode) {
            out.push(WorkEntry {
                path: idx.path.clone(),
                oid: idx.oid,
                mode: idx.mode,
            });
            continue;
        }
        let content = if meta.is_symlink() {
            std::fs::read_link(&abs)?
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            std::fs::read(&abs)?
        };
        out.push(WorkEntry {
            path: idx.path.clone(),
            oid: ObjectId::hash_object(algo, ObjectKind::Blob, &content),
            mode: wt_mode,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn scan_dir(
    root: &Path,
    dir: &Path,
    algo: HashAlgo,
    cache: Option<&StatCache<'_>>,
    out: &mut Vec<WorkEntry>,
) -> Result<(), WorktreeError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".alt" || name == ".git" {
            continue; // control directories are not working-tree content
        }
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            // A submodule directory carries its own git layout (a `.git`
            // file pointing at the gitdir, or a real `.git` dir). It's a
            // separate repo, not working-tree content of this one — don't
            // recurse, and don't synthesize a worktree entry for it: the
            // submodule's HEAD oid is owned by the submodule, not us.
            // status() filters spurious Deleted reports against the index.
            if is_submodule_dir(&path) {
                continue;
            }
            scan_dir(root, &path, algo, cache, out)?;
            continue;
        }
        let rel = path.strip_prefix(root).unwrap();
        let rel_b = rel_path(rel);
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
            std::fs::read(&path)?
        };
        out.push(WorkEntry {
            path: rel_b,
            oid: ObjectId::hash_object(algo, ObjectKind::Blob, &content),
            mode,
        });
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

/// Flattens a stored tree into path-sorted entries (full paths, recursing
/// sub-trees). Gitlinks are kept as entries; their target is opaque.
pub fn flatten_tree(
    odb: &NativeOdb,
    tree: ObjectId,
    algo: HashAlgo,
) -> Result<Vec<WorkEntry>, WorktreeError> {
    let mut out = Vec::new();
    flatten_into(odb, tree, algo, &mut Vec::new(), &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn flatten_into(
    odb: &NativeOdb,
    tree: ObjectId,
    algo: HashAlgo,
    prefix: &mut Vec<u8>,
    out: &mut Vec<WorkEntry>,
) -> Result<(), WorktreeError> {
    let obj = odb.get(&tree)?.ok_or(WorktreeError::NotATree(tree))?;
    if obj.kind != ObjectKind::Tree {
        return Err(WorktreeError::NotATree(tree));
    }
    for e in Tree::parse(&obj.data, algo)?.entries {
        let mark = prefix.len();
        if !prefix.is_empty() {
            prefix.push(b'/');
        }
        prefix.extend_from_slice(e.name.as_bytes());
        if e.mode.object_kind() == ObjectKind::Tree {
            flatten_into(odb, e.oid, algo, prefix, out)?;
        } else {
            out.push(WorkEntry {
                path: BString::from(prefix.clone()),
                oid: e.oid,
                mode: e.mode.value(),
            });
        }
        prefix.truncate(mark);
    }
    Ok(())
}

/// Builds the tree hierarchy for `entries` (each a file path + blob id +
/// mode), writing every (sub)tree object to `odb`, and returns the root
/// tree id. Entries need not be pre-sorted. The encoding matches git's
/// exactly (git tree ordering: a directory sorts as if its name ended in
/// `/`), so the resulting ids equal `git write-tree`'s.
pub fn write_tree(
    odb: &mut NativeOdb,
    entries: &[WorkEntry],
    algo: HashAlgo,
) -> Result<ObjectId, WorktreeError> {
    let mut flat: Vec<(&[u8], ObjectId, u32)> = entries
        .iter()
        .map(|e| (e.path.as_bytes(), e.oid, e.mode))
        .collect();
    flat.sort_by(|a, b| a.0.cmp(b.0));
    write_subtree(odb, &flat, algo)
}

fn write_subtree(
    odb: &mut NativeOdb,
    entries: &[(&[u8], ObjectId, u32)],
    algo: HashAlgo,
) -> Result<ObjectId, WorktreeError> {
    // (name, id, mode) entries of this level, files and built sub-trees
    let mut level: Vec<(Vec<u8>, ObjectId, u32)> = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        let (path, oid, mode) = entries[i];
        match path.iter().position(|&b| b == b'/') {
            None => {
                level.push((path.to_vec(), oid, mode));
                i += 1;
            }
            Some(slash) => {
                let dir = &path[..slash];
                // all entries under this directory are a contiguous run
                let mut group: Vec<(&[u8], ObjectId, u32)> = Vec::new();
                while i < entries.len() {
                    let p = entries[i].0;
                    if p.len() > slash && &p[..slash] == dir && p[slash] == b'/' {
                        group.push((&p[slash + 1..], entries[i].1, entries[i].2));
                        i += 1;
                    } else {
                        break;
                    }
                }
                let sub = write_subtree(odb, &group, algo)?;
                level.push((dir.to_vec(), sub, 0o040000));
            }
        }
    }

    // git tree order: compare names with a '/' appended for directories
    let key = |name: &[u8], mode: u32| {
        let mut k = name.to_vec();
        if mode == 0o040000 {
            k.push(b'/');
        }
        k
    };
    level.sort_by_key(|e| key(&e.0, e.2));

    let mut bytes = Vec::new();
    for (name, id, mode) in &level {
        bytes.extend_from_slice(format!("{mode:o}").as_bytes());
        bytes.push(b' ');
        bytes.extend_from_slice(name);
        bytes.push(0);
        bytes.extend_from_slice(id.as_bytes());
    }
    let id = ObjectId::hash_object(algo, ObjectKind::Tree, &bytes);
    odb.put(id, ObjectKind::Tree, &bytes)?;
    Ok(id)
}

/// A commit author/committer line: `Name <email> <unix-ts> <tz>`.
pub struct Sig<'a> {
    pub name: &'a str,
    pub email: &'a str,
    /// Seconds since the epoch.
    pub when: i64,
    /// Timezone like `+0000`.
    pub tz: &'a str,
}

/// Writes a commit object and returns its id. The bytes are git-canonical,
/// so the id equals `git commit-tree`'s for the same inputs.
pub fn write_commit(
    odb: &mut NativeOdb,
    tree: ObjectId,
    parents: &[ObjectId],
    author: &Sig,
    committer: &Sig,
    message: &str,
    algo: HashAlgo,
) -> Result<ObjectId, WorktreeError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("tree {tree}\n").as_bytes());
    for p in parents {
        bytes.extend_from_slice(format!("parent {p}\n").as_bytes());
    }
    let line = |s: &Sig| format!("{} <{}> {} {}", s.name, s.email, s.when, s.tz);
    bytes.extend_from_slice(format!("author {}\n", line(author)).as_bytes());
    bytes.extend_from_slice(format!("committer {}\n", line(committer)).as_bytes());
    bytes.push(b'\n');
    bytes.extend_from_slice(message.as_bytes());
    let id = ObjectId::hash_object(algo, ObjectKind::Commit, &bytes);
    odb.put(id, ObjectKind::Commit, &bytes)?;
    Ok(id)
}

/// The index's tracked entries (stage 0 only) as `WorkEntry`s.
pub fn index_entries(index: &Index) -> Vec<WorkEntry> {
    let mut out: Vec<WorkEntry> = index
        .entries
        .iter()
        .filter(|e| e.stage() == 0)
        .map(|e| WorkEntry {
            path: e.path.clone(),
            oid: e.oid,
            mode: e.mode,
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// One path that differs between two entry lists, carrying both sides so a
/// caller can fetch and diff their contents. `old`/`new` are `None` when the
/// path is absent from that side (a pure add or delete).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change<'a> {
    pub path: &'a BString,
    pub old: Option<&'a WorkEntry>,
    pub new: Option<&'a WorkEntry>,
}

/// Sorted-merge of two path-sorted entry lists into the set of differing
/// paths (added / deleted / oid-or-mode changed). Equal entries are dropped.
pub fn changes<'a>(old: &'a [WorkEntry], new: &'a [WorkEntry]) -> Vec<Change<'a>> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        match old[i].path.cmp(&new[j].path) {
            std::cmp::Ordering::Less => {
                out.push(Change {
                    path: &old[i].path,
                    old: Some(&old[i]),
                    new: None,
                });
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(Change {
                    path: &new[j].path,
                    old: None,
                    new: Some(&new[j]),
                });
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                if old[i].oid != new[j].oid || old[i].mode != new[j].mode {
                    out.push(Change {
                        path: &new[j].path,
                        old: Some(&old[i]),
                        new: Some(&new[j]),
                    });
                }
                i += 1;
                j += 1;
            }
        }
    }
    for e in &old[i..] {
        out.push(Change {
            path: &e.path,
            old: Some(e),
            new: None,
        });
    }
    for e in &new[j..] {
        out.push(Change {
            path: &e.path,
            old: None,
            new: Some(e),
        });
    }
    out
}

/// Three-way status. Inputs must each be sorted by path. `head` and `index`
/// drive the staged column (index vs HEAD); `index` and `worktree` drive the
/// unstaged column and the untracked list.
pub fn status(head: &[WorkEntry], index: &[WorkEntry], worktree: &[WorkEntry]) -> Status {
    let mut s = Status::default();
    diff(head, index, |path, kind| s.staged.push((path, kind)));
    // Gitlink paths in the index point at submodules: the worktree side
    // (scan_worktree) deliberately emits no entry for them, so a naive diff
    // would report them as Deleted on every status. Git treats an
    // uninitialised submodule directory as clean; mirror that by filtering
    // gitlink-tagged Deleted reports.
    let gitlinks: std::collections::HashSet<&BString> = index
        .iter()
        .filter(|e| e.mode == 0o160000)
        .map(|e| &e.path)
        .collect();
    diff(index, worktree, |path, kind| match kind {
        ChangeKind::Added => s.untracked.push(path),
        ChangeKind::Deleted if gitlinks.contains(&path) => {}
        other => s.unstaged.push((path, other)),
    });
    s
}

/// Sorted-merge diff of two path-sorted entry lists: present in `new` only =
/// Added; differing oid/mode = Modified; present in `old` only = Deleted.
fn diff(old: &[WorkEntry], new: &[WorkEntry], mut emit: impl FnMut(BString, ChangeKind)) {
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        match old[i].path.cmp(&new[j].path) {
            std::cmp::Ordering::Less => {
                emit(old[i].path.clone(), ChangeKind::Deleted);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                emit(new[j].path.clone(), ChangeKind::Added);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                if old[i].oid != new[j].oid || old[i].mode != new[j].mode {
                    emit(new[j].path.clone(), ChangeKind::Modified);
                }
                i += 1;
                j += 1;
            }
        }
    }
    for e in &old[i..] {
        emit(e.path.clone(), ChangeKind::Deleted);
    }
    for e in &new[j..] {
        emit(e.path.clone(), ChangeKind::Added);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(b: u8) -> ObjectId {
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &[b])
    }
    fn we(path: &str, content: u8, mode: u32) -> WorkEntry {
        WorkEntry {
            path: path.into(),
            oid: oid(content),
            mode,
        }
    }

    #[test]
    fn status_classifies_staged_unstaged_and_untracked() {
        // HEAD: a@1, b@1 ; index: a@2 (staged mod), c@1 (staged add), b gone
        // (staged del) ; worktree: a@2 (clean), c@3 (unstaged mod), d@1 (new)
        let head = vec![we("a", 1, 0o100644), we("b", 1, 0o100644)];
        let index = vec![we("a", 2, 0o100644), we("c", 1, 0o100644)];
        let wt = vec![
            we("a", 2, 0o100644),
            we("c", 3, 0o100644),
            we("d", 1, 0o100644),
        ];
        let s = status(&head, &index, &wt);
        assert_eq!(
            s.staged,
            vec![
                ("a".into(), ChangeKind::Modified),
                ("b".into(), ChangeKind::Deleted),
                ("c".into(), ChangeKind::Added),
            ]
        );
        assert_eq!(s.unstaged, vec![("c".into(), ChangeKind::Modified)]);
        assert_eq!(s.untracked, vec![BString::from("d")]);
    }

    #[test]
    fn a_mode_change_is_a_modification() {
        let head = vec![we("x", 1, 0o100644)];
        let index = vec![we("x", 1, 0o100755)]; // same content, +x
        let s = status(&head, &index, &index);
        assert_eq!(s.staged, vec![("x".into(), ChangeKind::Modified)]);
        assert!(s.unstaged.is_empty());
    }

    #[test]
    fn scan_reads_files_exec_symlink_and_skips_control_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), b"hello").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/b.txt"), b"nested").unwrap();
        std::fs::create_dir(root.join(".alt")).unwrap();
        std::fs::write(root.join(".alt/junk"), b"ignore me").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            std::fs::write(root.join("run.sh"), b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            symlink("a.txt", root.join("link")).unwrap();
        }

        let scan = scan_worktree(root, HashAlgo::Sha1).unwrap();
        let by_path: std::collections::HashMap<_, _> =
            scan.iter().map(|e| (e.path.to_string(), e)).collect();
        assert!(!by_path.contains_key(".alt/junk"), "control dir skipped");
        assert_eq!(by_path["a.txt"].mode, 0o100644);
        assert_eq!(by_path["sub/b.txt"].path, "sub/b.txt");
        assert_eq!(
            by_path["a.txt"].oid,
            ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"hello")
        );
        #[cfg(unix)]
        {
            assert_eq!(by_path["run.sh"].mode, 0o100755, "exec bit");
            assert_eq!(by_path["link"].mode, 0o120000, "symlink");
            assert_eq!(
                by_path["link"].oid,
                ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"a.txt"),
                "symlink hashes its target"
            );
        }
    }

    #[test]
    fn scan_treats_submodule_dir_as_opaque() {
        // a submodule worktree carries a .git entry (file or dir); scan
        // must not descend into it — the inner files belong to another
        // repo, not the parent's worktree.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("top.txt"), b"hi").unwrap();
        let sub = root.join("submod");
        std::fs::create_dir(&sub).unwrap();
        std::fs::create_dir(sub.join(".git")).unwrap();
        std::fs::write(sub.join(".git").join("HEAD"), b"ref: refs/heads/x\n").unwrap();
        std::fs::write(sub.join("inner.txt"), b"should be invisible").unwrap();

        let scan = scan_worktree(root, HashAlgo::Sha1).unwrap();
        let paths: Vec<_> = scan.iter().map(|e| e.path.to_string()).collect();
        assert_eq!(paths, vec!["top.txt".to_owned()]);
    }

    #[test]
    fn status_treats_index_gitlink_as_clean_when_worktree_lacks_path() {
        // A submodule that's recorded in the index but not initialised on
        // disk should not be reported as "deleted" by status — git's
        // baseline behaviour for an uninitialised submodule is clean.
        let gitlink = WorkEntry {
            path: "shFlags".into(),
            oid: oid(0xAA),
            mode: 0o160000,
        };
        let head = vec![gitlink.clone()];
        let index = vec![gitlink];
        let wt: Vec<WorkEntry> = vec![];
        let s = status(&head, &index, &wt);
        assert!(s.staged.is_empty(), "no staged changes expected");
        assert!(
            s.unstaged.is_empty(),
            "uninit submodule must not show as deleted"
        );
        assert!(s.untracked.is_empty());
    }

    #[test]
    fn flatten_recurses_subtrees_with_full_paths() {
        let dir = tempfile::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let (oid_a, oid_b) = (oid(0xAA), oid(0xBB));

        // a sub-tree holding b.txt, then a root tree with a.txt + the subtree
        let put_tree = |odb: &mut NativeOdb, entries: &[(&str, &str, &ObjectId)]| -> ObjectId {
            let mut bytes = Vec::new();
            for (mode, name, oid) in entries {
                bytes.extend_from_slice(mode.as_bytes());
                bytes.push(b' ');
                bytes.extend_from_slice(name.as_bytes());
                bytes.push(0);
                bytes.extend_from_slice(oid.as_bytes());
            }
            let id = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &bytes);
            odb.put(id, ObjectKind::Tree, &bytes).unwrap();
            id
        };
        let sub = put_tree(&mut odb, &[("100644", "b.txt", &oid_b)]);
        let root = put_tree(
            &mut odb,
            &[("100644", "a.txt", &oid_a), ("40000", "sub", &sub)],
        );
        odb.flush().unwrap();

        let flat = flatten_tree(&odb, root, HashAlgo::Sha1).unwrap();
        assert_eq!(
            flat,
            vec![
                WorkEntry {
                    path: "a.txt".into(),
                    oid: oid_a,
                    mode: 0o100644
                },
                WorkEntry {
                    path: "sub/b.txt".into(),
                    oid: oid_b,
                    mode: 0o100644
                },
            ]
        );
    }
}
