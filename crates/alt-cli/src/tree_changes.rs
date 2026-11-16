//! Differences between two trees: the changed files, and which removed
//! and added files are one file renamed.

use alt_bytes::BString;
use alt_git_codec::{ObjectId, ObjectKind, Tree};
use alt_repo::Repository;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// One file in a tree: its oid and mode. The
/// path is threaded separately (as a borrowed slice into the prefix
/// buffer) because `tree_diff` already has it on hand and stashing a
/// fresh `BString` per leaf would dominate the cost of a small patch.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct TreeFile {
    pub(crate) oid: ObjectId,
    pub(crate) mode: u32,
}

/// Recursively diffs two trees, emitting only the changed leaves. Equal
/// (same-oid) subtrees are skipped wholesale — that's the whole point: a
/// commit that touched 1-3 files in a 50k-file monorepo shares every
/// subtree but a single deep path with its parent, and we descend only
/// into the differing branches. O(changes × depth) tree reads instead of
/// O(whole-tree) flatten-both-then-merge.
type TreeEmit<'a> = dyn FnMut(&[u8], Option<TreeFile>, Option<TreeFile>) -> Res<()> + 'a;

fn tree_diff(
    repo: &Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    prefix: &mut Vec<u8>,
    emit: &mut TreeEmit<'_>,
) -> Res<()> {
    if old == new {
        return Ok(()); // identical subtree (or both absent) — nothing to emit
    }
    let read_tree = |oid: ObjectId| -> Res<Vec<alt_git_codec::TreeEntry>> {
        let obj = repo
            .read_object(&oid)?
            .ok_or_else(|| format!("tree {oid} missing"))?;
        if obj.kind != ObjectKind::Tree {
            return Err(format!("{oid} is not a tree").into());
        }
        Ok(Tree::parse(&obj.data, repo.algo()?)?.entries)
    };

    let old_entries = match old {
        Some(o) => read_tree(o)?,
        None => Vec::new(),
    };
    let new_entries = match new {
        Some(n) => read_tree(n)?,
        None => Vec::new(),
    };

    let (mut i, mut j) = (0, 0);
    while i < old_entries.len() || j < new_entries.len() {
        let cmp = match (old_entries.get(i), new_entries.get(j)) {
            (Some(o), Some(n)) => o.tree_order(n),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => break,
        };
        type PushBody<'a> = dyn FnMut(&mut Vec<u8>) -> Res<()> + 'a;
        let push = |prefix: &mut Vec<u8>, name: &[u8], f: &mut PushBody<'_>| -> Res<()> {
            let mark = prefix.len();
            if !prefix.is_empty() {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(name);
            let r = f(prefix);
            prefix.truncate(mark);
            r
        };
        match cmp {
            std::cmp::Ordering::Less => {
                let o = old_entries[i].clone();
                push(prefix, o.name.as_slice(), &mut |p| {
                    handle_side(repo, p, &o, true, emit)
                })?;
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                let n = new_entries[j].clone();
                push(prefix, n.name.as_slice(), &mut |p| {
                    handle_side(repo, p, &n, false, emit)
                })?;
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                let o = old_entries[i].clone();
                let n = new_entries[j].clone();
                if o.oid == n.oid && o.mode == n.mode {
                    i += 1;
                    j += 1;
                    continue;
                }
                // equal in git's order: both are directories, or neither is
                push(prefix, n.name.as_slice(), &mut |p| {
                    if o.mode.object_kind() == ObjectKind::Tree {
                        return tree_diff(repo, Some(o.oid), Some(n.oid), p, emit);
                    }
                    let leaf = |e: &alt_git_codec::TreeEntry| TreeFile {
                        oid: e.oid,
                        mode: e.mode.value(),
                    };
                    emit(p, Some(leaf(&o)), Some(leaf(&n)))
                })?;
                i += 1;
                j += 1;
            }
        }
    }
    Ok(())
}

/// For an entry that exists on only one side (pure add or pure delete),
/// emit its leaves: if it's a tree, recurse into it with the other side
/// absent; otherwise emit the single leaf.
fn handle_side(
    repo: &Repository,
    prefix: &mut Vec<u8>,
    entry: &alt_git_codec::TreeEntry,
    deleted: bool,
    emit: &mut TreeEmit<'_>,
) -> Res<()> {
    if entry.mode.object_kind() == ObjectKind::Tree {
        let (old, new) = if deleted {
            (Some(entry.oid), None)
        } else {
            (None, Some(entry.oid))
        };
        return tree_diff(repo, old, new, prefix, emit);
    }
    let leaf = TreeFile {
        oid: entry.oid,
        mode: entry.mode.value(),
    };
    if deleted {
        emit(prefix, Some(leaf), None)
    } else {
        emit(prefix, None, Some(leaf))
    }
}

/// A path that differs between two trees.
pub(crate) struct Changed {
    pub(crate) path: BString,
    pub(crate) old: Option<TreeFile>,
    pub(crate) new: Option<TreeFile>,
}

/// The files that differ between two trees, in path order.
pub(crate) fn changes(
    repo: &Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
) -> Res<Vec<Changed>> {
    let mut out = Vec::new();
    let mut prefix = Vec::new();
    let mut emit = |path: &[u8], old: Option<TreeFile>, new: Option<TreeFile>| -> Res<()> {
        out.push(Changed {
            path: BString::from(path.to_vec()),
            old,
            new,
        });
        Ok(())
    };
    tree_diff(repo, old, new, &mut prefix, &mut emit)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// One entry of a diff once renames are paired: a file changed in place,
/// added, removed, or moved from `old_path` to `path`.
pub(crate) struct FileDiff {
    pub(crate) path: BString,
    /// where the old side lived, when not at `path`
    pub(crate) old_path: Option<BString>,
    pub(crate) old: Option<TreeFile>,
    pub(crate) new: Option<TreeFile>,
    /// similarity of a renamed file, in percent
    pub(crate) similarity: Option<u32>,
}

/// Pairs removed files with added files that are the same file renamed.
/// Each pair is reported at its new path, in path order with the rest.
pub(crate) fn pair_renames(repo: &Repository, changes: Vec<Changed>) -> Res<Vec<FileDiff>> {
    use alt_diff::rename::File;
    let side = |want_removed: bool| -> Vec<usize> {
        (0..changes.len())
            .filter(|&i| match (&changes[i].old, &changes[i].new) {
                (Some(_), None) => want_removed,
                (None, Some(_)) => !want_removed,
                _ => false,
            })
            .collect()
    };
    let (removed, added) = (side(true), side(false));
    let file = |i: &usize| {
        let c = &changes[*i];
        let f = c.old.as_ref().or(c.new.as_ref()).expect("one side exists");
        File {
            path: c.path.as_slice(),
            id: f.oid.as_bytes(),
            mode: f.mode,
        }
    };
    let sources: Vec<File<'_>> = removed.iter().map(file).collect();
    let targets: Vec<File<'_>> = added.iter().map(file).collect();
    let mut load = |source: bool, i: usize| -> Res<Vec<u8>> {
        let c = &changes[if source { removed[i] } else { added[i] }];
        let f = c.old.as_ref().or(c.new.as_ref()).expect("one side exists");
        Ok(repo
            .read_object(&f.oid)?
            .ok_or_else(|| format!("blob {} missing", f.oid))?
            .data)
    };
    let renames = alt_diff::rename::detect(&sources, &targets, &mut load)?;
    let mut moved_from: std::collections::HashMap<usize, (usize, u32)> = renames
        .iter()
        .map(|r| (added[r.dst], (removed[r.src], r.percent())))
        .collect();
    let gone: std::collections::HashSet<usize> = renames.iter().map(|r| removed[r.src]).collect();
    let mut out = Vec::with_capacity(changes.len());
    for (i, c) in changes.iter().enumerate() {
        if gone.contains(&i) {
            continue;
        }
        let from = moved_from.remove(&i);
        out.push(FileDiff {
            path: c.path.clone(),
            old_path: from.map(|(src, _)| changes[src].path.clone()),
            old: from.map_or(c.old, |(src, _)| changes[src].old),
            new: c.new,
            similarity: from.map(|(_, percent)| percent),
        });
    }
    Ok(out)
}

/// The file at `path` in a tree, when the path is a file there. Only the
/// trees on the way are read, and none of them parsed in full.
pub(crate) fn file_at(repo: &Repository, tree: ObjectId, path: &[u8]) -> Res<Option<TreeFile>> {
    let mut current = tree;
    let mut parts = path.split(|&b| b == b'/').peekable();
    while let Some(part) = parts.next() {
        let obj = repo
            .read_object(&current)?
            .ok_or_else(|| format!("tree {current} missing"))?;
        let Some((mode, oid)) = Tree::find(&obj.data, repo.algo()?, part)? else {
            return Ok(None);
        };
        match (parts.peek().is_some(), mode == 0o040000) {
            (true, true) => current = oid,
            (false, false) => return Ok(Some(TreeFile { oid, mode })),
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// Resolve `<commit>:<path>` to a blob's bytes. Walks the commit's
/// tree segment-by-segment, refusing to descend past a non-tree entry
/// and refusing the final entry if it's a tree (use `alt cat-file -p`
/// for tree dumps).
pub(crate) fn blob_at(
    repo: &Repository,
    commit_oid: ObjectId,
    path: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let commit = repo.read_commit(&commit_oid)?;
    let mut current = commit
        .tree()
        .ok_or_else(|| format!("commit {commit_oid} has no tree"))?;
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err("empty path".into());
    }
    for (i, seg) in segments.iter().enumerate() {
        let obj = repo
            .read_object(&current)?
            .ok_or_else(|| format!("tree {current} missing"))?;
        if obj.kind != ObjectKind::Tree {
            return Err(format!("{current} is not a tree").into());
        }
        let tree = Tree::parse(&obj.data, repo.algo()?)?;
        let entry = tree
            .entries
            .iter()
            .find(|e| String::from_utf8_lossy(e.name.as_slice()) == **seg)
            .ok_or_else(|| format!("path '{path}' not found at {commit_oid}"))?;
        if i == segments.len() - 1 {
            if entry.mode.value() == 0o040000 {
                return Err(format!("'{path}' is a tree, not a blob").into());
            }
            let blob_obj = repo
                .read_object(&entry.oid)?
                .ok_or_else(|| format!("blob {} missing", entry.oid))?;
            return Ok(blob_obj.data);
        }
        if entry.mode.value() != 0o040000 {
            return Err(format!("'{seg}' is not a directory in path '{path}'").into());
        }
        current = entry.oid;
    }
    Err("unreachable".into())
}
