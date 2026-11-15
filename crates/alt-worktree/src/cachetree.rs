//! The trees the index's directories make, and the comparison of a stored
//! tree with the index that only opens the directories where they differ.

use alt_bytes::{BString, ByteSlice};
use alt_git_codec::{ObjectId, ObjectKind, Tree};
use alt_odb::NativeOdb;

use crate::snapshot::Snapshot;
use crate::{WorkEntry, WorktreeError};

/// A path that differs between a tree and the index; a side is `None`
/// where the path is absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub path: BString,
    pub old: Option<WorkEntry>,
    pub new: Option<WorkEntry>,
}

impl Snapshot {
    /// Works out the tree of every directory that has none yet. A
    /// directory with a conflicted path beneath it makes no tree.
    pub fn fill_trees(&mut self) {
        for d in (0..self.dirs.len()).rev() {
            if self.dirs[d].tree.is_some() {
                continue;
            }
            if let Some(bytes) = self.tree_bytes(d) {
                self.dirs[d].tree =
                    Some(ObjectId::hash_object(self.algo, ObjectKind::Tree, &bytes));
                self.changed = true;
            }
        }
    }

    fn tree_bytes(&self, d: usize) -> Option<Vec<u8>> {
        let skip = match self.dirs[d].path_len {
            0 => 0,
            len => len as usize + 1,
        };
        let mut bytes = Vec::new();
        let mut put = |mode: u32, name: &[u8], oid: &ObjectId| {
            bytes.extend_from_slice(format!("{mode:o} ").as_bytes());
            bytes.extend_from_slice(name);
            bytes.push(0);
            bytes.extend_from_slice(oid.as_bytes());
        };
        let mut dirs = self.child_dirs(d).peekable();
        let mut at = self.dirs[d].first as usize;
        while at < self.dirs[d].end as usize {
            if let Some(&c) = dirs.peek()
                && self.dirs[c].first as usize == at
            {
                dirs.next();
                put(
                    0o40000,
                    &self.dir_path(c)[skip..],
                    self.dirs[c].tree.as_ref()?,
                );
                at = self.dirs[c].end as usize;
                continue;
            }
            let e = &self.entries[at];
            if e.stage() != 0 {
                return None;
            }
            put(e.mode, &self.path(at)[skip..], &e.oid);
            at += 1;
        }
        Some(bytes)
    }

    fn work_entry(&self, i: usize) -> WorkEntry {
        WorkEntry {
            path: BString::from(self.path(i)),
            oid: self.entries[i].oid,
            mode: self.entries[i].mode,
        }
    }

    /// The paths where the index differs from the stored tree `tree` (from
    /// nothing, for `None`), in path order. Conflicted entries count as
    /// absent from the index.
    pub fn staged(
        &self,
        odb: &NativeOdb,
        tree: Option<ObjectId>,
    ) -> Result<Vec<Staged>, WorktreeError> {
        let mut out = Vec::new();
        self.staged_in(odb, tree, Some(0), &mut Vec::new(), &mut out)?;
        Ok(out)
    }

    fn staged_in(
        &self,
        odb: &NativeOdb,
        tree: Option<ObjectId>,
        dir: Option<usize>,
        prefix: &mut Vec<u8>,
        out: &mut Vec<Staged>,
    ) -> Result<(), WorktreeError> {
        if let (Some(tree), Some(d)) = (tree, dir)
            && self.dirs[d].tree == Some(tree)
        {
            return Ok(());
        }
        let stored = match tree {
            Some(id) => {
                let obj = odb.get(&id)?.ok_or(WorktreeError::NotATree(id))?;
                if obj.kind != ObjectKind::Tree {
                    return Err(WorktreeError::NotATree(id));
                }
                Tree::parse(&obj.data, self.algo)?.entries
            }
            None => Vec::new(),
        };
        // the index side: (name, entry or directory), in the same order
        let mut indexed: Vec<(&[u8], Result<usize, usize>)> = Vec::new();
        if let Some(d) = dir {
            let skip = match self.dirs[d].path_len {
                0 => 0,
                len => len as usize + 1,
            };
            let mut dirs = self.child_dirs(d).peekable();
            let mut at = self.dirs[d].first as usize;
            while at < self.dirs[d].end as usize {
                if let Some(&c) = dirs.peek()
                    && self.dirs[c].first as usize == at
                {
                    dirs.next();
                    indexed.push((&self.dir_path(c)[skip..], Err(c)));
                    at = self.dirs[c].end as usize;
                    continue;
                }
                if self.entries[at].stage() == 0 {
                    indexed.push((&self.path(at)[skip..], Ok(at)));
                }
                at += 1;
            }
        }
        let is_tree = |e: &alt_git_codec::TreeEntry| e.mode.object_kind() == ObjectKind::Tree;
        let (mut i, mut j) = (0, 0);
        while i < stored.len() || j < indexed.len() {
            let order = match (stored.get(i), indexed.get(j)) {
                (Some(s), Some((name, kind))) => {
                    key_cmp(s.name.as_bytes(), is_tree(s), name, kind.is_err())
                }
                (Some(_), None) => std::cmp::Ordering::Less,
                _ => std::cmp::Ordering::Greater,
            };
            let old = (order != std::cmp::Ordering::Greater).then(|| &stored[i]);
            let new = (order != std::cmp::Ordering::Less).then(|| indexed[j]);
            i += usize::from(old.is_some());
            j += usize::from(new.is_some());
            let name = old
                .map(|s| s.name.as_bytes())
                .or(new.map(|n| n.0))
                .expect("one side");
            let mark = prefix.len();
            if mark > 0 {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(name);
            let old_tree = old.filter(|s| is_tree(s));
            let new_dir = new.and_then(|n| n.1.err());
            if old_tree.is_some() || new_dir.is_some() {
                self.staged_in(odb, old_tree.map(|s| s.oid), new_dir, prefix, out)?;
            } else {
                let old = old.map(|s| WorkEntry {
                    path: BString::from(prefix.as_slice()),
                    oid: s.oid,
                    mode: s.mode.value(),
                });
                let new = new.and_then(|n| n.1.ok()).map(|e| self.work_entry(e));
                if old != new {
                    out.push(Staged {
                        path: BString::from(prefix.as_slice()),
                        old,
                        new,
                    });
                }
            }
            prefix.truncate(mark);
        }
        Ok(())
    }
}

/// Orders names the way the index orders the paths beneath them: a
/// directory counts as its name followed by a slash.
pub(crate) fn key_cmp(a: &[u8], a_dir: bool, b: &[u8], b_dir: bool) -> std::cmp::Ordering {
    let n = a.len().min(b.len());
    a[..n].cmp(&b[..n]).then_with(|| {
        let next = |name: &[u8], dir: bool| name.get(n).copied().or(dir.then_some(b'/'));
        next(a, a_dir).cmp(&next(b, b_dir))
    })
}
