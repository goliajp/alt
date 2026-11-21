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
                let tree = ObjectId::hash_object(self.algo, ObjectKind::Tree, &bytes);
                self.dirs[d].tree = Some(tree);
                self.changed = true;
                self.note_tree(d, &tree);
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

#[cfg(test)]
mod tests {
    use super::*;
    use alt_git_codec::HashAlgo;
    use alt_git_index::{Index, IndexEntry};

    const ALGO: HashAlgo = HashAlgo::Sha1;

    fn blob(content: &str) -> ObjectId {
        ObjectId::hash_object(ALGO, ObjectKind::Blob, content.as_bytes())
    }

    /// An entry at `path` for the blob of `content`, at `stage`.
    fn entry(path: &str, content: &str, mode: u32, stage: u16) -> IndexEntry {
        IndexEntry {
            ctime: (0, 0),
            mtime: (0, 0),
            dev: 0,
            ino: 0,
            mode,
            uid: 0,
            gid: 0,
            size: 0,
            oid: blob(content),
            flags: path.len() as u16 | stage << 12,
            extended_flags: None,
            path: path.into(),
        }
    }

    fn snapshot(mut entries: Vec<IndexEntry>) -> Snapshot {
        entries.sort_by(|a, b| a.path.cmp(&b.path).then(a.stage().cmp(&b.stage())));
        let index = Index {
            version: 2,
            entries,
            extensions: Vec::new(),
        };
        let mut snapshot = Snapshot::of_index(&index, b"id", ALGO, None);
        snapshot.fill_trees();
        snapshot
    }

    fn files(files: &[(&str, &str)]) -> Vec<IndexEntry> {
        files
            .iter()
            .map(|(path, content)| entry(path, content, 0o100644, 0))
            .collect()
    }

    /// The bytes of a tree object with these `(mode, name, id)` entries.
    fn tree_bytes(entries: &[(&str, &str, ObjectId)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (mode, name, oid) in entries {
            bytes.extend_from_slice(format!("{mode} {name}\0").as_bytes());
            bytes.extend_from_slice(oid.as_bytes());
        }
        bytes
    }

    fn tree_id(entries: &[(&str, &str, ObjectId)]) -> ObjectId {
        ObjectId::hash_object(ALGO, ObjectKind::Tree, &tree_bytes(entries))
    }

    fn put(odb: &mut NativeOdb, entries: &[(&str, &str, ObjectId)]) -> ObjectId {
        let bytes = tree_bytes(entries);
        let id = ObjectId::hash_object(ALGO, ObjectKind::Tree, &bytes);
        odb.put(id, ObjectKind::Tree, &bytes).unwrap();
        id
    }

    fn tree_of<'a>(s: &'a Snapshot, dir: &str) -> Option<&'a ObjectId> {
        let at = (0..s.dirs.len()).find(|&d| s.dir_path(d) == dir.as_bytes())?;
        s.dirs[at].tree.as_ref()
    }

    const TREE: [(&str, &str); 5] = [
        ("a.txt", "1"),
        ("a/b", "2"),
        ("a/c/d", "3"),
        ("run.sh", "4"),
        ("z/y", "5"),
    ];

    /// The trees `TREE` makes, by hand: `a` sorts as `a/`, after `a.txt`.
    fn trees_by_hand() -> [ObjectId; 4] {
        let c = tree_id(&[("100644", "d", blob("3"))]);
        let a = tree_id(&[("100644", "b", blob("2")), ("40000", "c", c)]);
        let z = tree_id(&[("100644", "y", blob("5"))]);
        let root = tree_id(&[
            ("100644", "a.txt", blob("1")),
            ("40000", "a", a),
            ("100755", "run.sh", blob("4")),
            ("40000", "z", z),
        ]);
        [root, a, c, z]
    }

    fn whole() -> Vec<IndexEntry> {
        let mut entries = files(&TREE);
        entries[3].mode = 0o100755;
        entries
    }

    #[test]
    fn every_directory_gets_the_id_of_the_tree_its_entries_make() {
        let s = snapshot(whole());
        let [root, a, c, z] = trees_by_hand();
        assert_eq!(tree_of(&s, ""), Some(&root));
        assert_eq!(tree_of(&s, "a"), Some(&a));
        assert_eq!(tree_of(&s, "a/c"), Some(&c));
        assert_eq!(tree_of(&s, "z"), Some(&z));
    }

    #[test]
    fn a_conflicted_path_leaves_the_directories_above_it_without_a_tree() {
        let mut entries = whole();
        entries.retain(|e| e.path != "a/c/d");
        for stage in 1..=3 {
            entries.push(entry("a/c/d", &stage.to_string(), 0o100644, stage));
        }
        let s = snapshot(entries);
        assert_eq!(tree_of(&s, "a/c"), None);
        assert_eq!(tree_of(&s, "a"), None);
        assert_eq!(tree_of(&s, ""), None);
        assert_eq!(tree_of(&s, "z"), Some(&trees_by_hand()[3]));
    }

    #[test]
    fn a_tree_the_index_matches_is_not_opened() {
        let dir = alt_os::temp::tempdir().unwrap();
        let odb = NativeOdb::open(dir.path()).unwrap();
        // the store holds no tree at all: equal ids are all it takes
        let s = snapshot(whole());
        let root = trees_by_hand()[0];
        assert_eq!(s.staged(&odb, Some(root)).unwrap(), Vec::new());
    }

    #[test]
    fn only_the_directories_that_differ_are_opened() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let [_, a, _, z] = trees_by_hand();
        // HEAD differs in z/y alone; `a` and what lies below are not stored
        let old_z = put(&mut odb, &[("100644", "y", blob("old"))]);
        let head = put(
            &mut odb,
            &[
                ("100644", "a.txt", blob("1")),
                ("40000", "a", a),
                ("100755", "run.sh", blob("4")),
                ("40000", "z", old_z),
            ],
        );
        assert_ne!(old_z, z);
        let staged = snapshot(whole()).staged(&odb, Some(head)).unwrap();
        let side = |content: &str| {
            Some(WorkEntry {
                path: "z/y".into(),
                oid: blob(content),
                mode: 0o100644,
            })
        };
        assert_eq!(
            staged,
            vec![Staged {
                path: "z/y".into(),
                old: side("old"),
                new: side("5"),
            }]
        );
    }

    #[test]
    fn added_removed_retyped_and_conflicted_paths_are_told_apart() {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut odb = NativeOdb::open(dir.path()).unwrap();
        let sub = put(&mut odb, &[("100644", "inner", blob("i"))]);
        let head = put(
            &mut odb,
            &[
                ("100644", "conflicted", blob("c")),
                ("100644", "exec", blob("e")),
                ("100644", "gone", blob("g")),
                ("100644", "same", blob("s")),
                ("40000", "turned", sub),
            ],
        );
        let mut entries = files(&[
            ("exec", "e"),
            ("new/file", "n"),
            ("same", "s"),
            ("turned", "now a file"),
        ]);
        entries[0].mode = 0o100755;
        entries.push(entry("conflicted", "ours", 0o100644, 2));
        entries.push(entry("conflicted", "theirs", 0o100644, 3));
        let staged = snapshot(entries).staged(&odb, Some(head)).unwrap();
        let seen: Vec<(String, Option<u32>, Option<u32>)> = staged
            .iter()
            .map(|s| {
                let mode = |w: &Option<WorkEntry>| w.as_ref().map(|w| w.mode);
                (s.path.to_string(), mode(&s.old), mode(&s.new))
            })
            .collect();
        let row = |path: &str, old, new| (path.to_owned(), old, new);
        assert_eq!(
            seen,
            vec![
                // a conflicted path counts as absent from the index
                row("conflicted", Some(0o100644), None),
                row("exec", Some(0o100644), Some(0o100755)),
                row("gone", Some(0o100644), None),
                row("new/file", None, Some(0o100644)),
                row("turned", None, Some(0o100644)),
                row("turned/inner", Some(0o100644), None),
            ]
        );
        // against no tree at all every staged path is new
        let all = snapshot(files(&TREE)).staged(&odb, None).unwrap();
        assert_eq!(all.len(), TREE.len());
        assert!(all.iter().all(|s| s.old.is_none() && s.new.is_some()));
    }
}
