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
/// Returns entries sorted by path.
pub fn scan_worktree(root: &Path, algo: HashAlgo) -> Result<Vec<WorkEntry>, WorktreeError> {
    let mut out = Vec::new();
    scan_dir(root, root, algo, &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn scan_dir(
    root: &Path,
    dir: &Path,
    algo: HashAlgo,
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
            scan_dir(root, &path, algo, out)?;
            continue;
        }
        let (mode, content) = if meta.is_symlink() {
            let target = std::fs::read_link(&path)?;
            (0o120000, target.as_os_str().as_encoded_bytes().to_vec())
        } else {
            let content = std::fs::read(&path)?;
            (file_mode(&meta), content)
        };
        let rel = path.strip_prefix(root).unwrap();
        out.push(WorkEntry {
            path: rel_path(rel),
            oid: ObjectId::hash_object(algo, ObjectKind::Blob, &content),
            mode,
        });
    }
    Ok(())
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
    diff(index, worktree, |path, kind| match kind {
        // a worktree file with no index entry is untracked, not "added"
        ChangeKind::Added => s.untracked.push(path),
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
