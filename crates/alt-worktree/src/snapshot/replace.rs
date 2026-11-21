//! The snapshot of an index made from the snapshot of the index it
//! replaced and the paths at which the two differ, without a pass over
//! the entries that stayed.

use alt_git_codec::ObjectId;

use super::{Entry, Snapshot};

/// What an index has at a path where the index before it had something
/// else: the mode, flags and content of each entry, in the order of their
/// stages; none where the path is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replaced<'a> {
    pub path: &'a [u8],
    pub entries: Vec<(u32, u16, ObjectId)>,
}

/// the bits of an entry's flags that are not the length of its name
const KEPT_FLAGS: u16 = 0xf000;

impl Snapshot {
    /// Makes this the snapshot of the index that has `changes`, in the
    /// order of their paths, where the index this is the snapshot of has
    /// something else, and whose file ends in the checksum `index_id`:
    /// what [`of_index`](Self::of_index) makes of that index with this
    /// snapshot as the prior one.
    pub fn replace_paths(&mut self, index_id: &[u8], changes: &[Replaced<'_>]) {
        self.note_index(index_id, changes);
        self.index_id = index_id.to_vec();
        self.changed = true;
        // entries are changed where they stand while every path keeps
        // the number of its entries; from the first that does not, the
        // entries are laid out again
        for (done, change) in changes.iter().enumerate() {
            let (lo, hi) = self.entries_at(change.path, 0);
            if hi - lo != change.entries.len() {
                return self.lay_out(&changes[done..]);
            }
            let mut differs = false;
            for (entry, new) in self.entries[lo..hi].iter_mut().zip(&change.entries) {
                if !holds(entry, new) {
                    (entry.mode, entry.oid, entry.stat) = (new.0, new.2, None);
                    differs = true;
                }
                entry.flags = new.1;
            }
            if differs {
                self.forget_trees_over(lo);
            }
        }
    }

    /// The entries at `path` among those from `from` on, as a range.
    fn entries_at(&self, path: &[u8], from: usize) -> (usize, usize) {
        let of = |e: &Entry| &self.paths[e.path.0 as usize..(e.path.0 + e.path.1) as usize];
        let lo = from + self.entries[from..].partition_point(|e| of(e) < path);
        let same = self.entries[lo..].iter().take_while(|e| of(e) == path);
        (lo, lo + same.count())
    }

    /// Takes the tree from every directory entry `at` lies in.
    fn forget_trees_over(&mut self, at: usize) {
        let within = |s: &Snapshot, d: usize| {
            (s.dirs[d].first as usize..s.dirs[d].end as usize).contains(&at)
        };
        let mut dir = Some(0);
        while let Some(d) = dir {
            self.dirs[d].tree = None;
            dir = self.child_dirs(d).find(|&child| within(self, child));
        }
    }

    /// [`replace_paths`](Self::replace_paths) where entries come and go:
    /// the entries in a new row, and the directories made from it, each
    /// with its tree where it has the entries it had.
    fn lay_out(&mut self, changes: &[Replaced<'_>]) {
        let old = std::mem::take(&mut self.entries);
        let mut entries = Vec::with_capacity(old.len() + changes.len());
        // which of the old entries each entry is, when it is one unchanged
        let mut same: Vec<Option<u32>> = Vec::with_capacity(old.len() + changes.len());
        let mut at = 0;
        self.entries = old;
        for change in changes {
            let (lo, hi) = self.entries_at(change.path, at);
            entries.extend_from_slice(&self.entries[at..lo]);
            same.extend((at..lo).map(|i| Some(i as u32)));
            let path = match lo < hi {
                true => self.entries[lo].path,
                false => {
                    let path = (self.paths.len() as u32, change.path.len() as u32);
                    self.paths.extend_from_slice(change.path);
                    path
                }
            };
            for new in &change.entries {
                let stage = |flags: u16| flags & 0x3000;
                let was = (lo..hi).find(|&i| stage(self.entries[i].flags) == stage(new.1));
                let was = was.filter(|&i| holds(&self.entries[i], new));
                same.push(was.map(|i| i as u32));
                entries.push(Entry {
                    path,
                    oid: new.2,
                    mode: new.0,
                    flags: new.1,
                    stat: was.and_then(|i| self.entries[i].stat),
                });
            }
            at = hi;
        }
        entries.extend_from_slice(&self.entries[at..]);
        same.extend((at..self.entries.len()).map(|i| Some(i as u32)));
        self.entries = entries;
        let prior = std::mem::take(&mut self.dirs);
        self.build_dirs();
        self.keep_trees(&prior, &same);
    }
}

/// Whether `entry` holds what `new` says an entry at its path has.
fn holds(entry: &Entry, new: &(u32, u16, ObjectId)) -> bool {
    entry.oid == new.2 && entry.mode == new.0 && entry.flags & KEPT_FLAGS == new.1 & KEPT_FLAGS
}

#[cfg(test)]
mod tests {
    use super::super::tests::{oid, stat};
    use super::*;
    use alt_git_codec::HashAlgo;
    use alt_git_index::{Index, IndexEntry};

    const ALGO: HashAlgo = HashAlgo::Sha1;

    fn entry(path: &str, stage: u16, content: u8) -> IndexEntry {
        IndexEntry {
            ctime: (0, 0),
            mtime: (0, 0),
            dev: 0,
            ino: 0,
            mode: if content.is_multiple_of(5) {
                0o100755
            } else {
                0o100644
            },
            uid: 0,
            gid: 0,
            size: 0,
            oid: oid(content),
            flags: path.len() as u16 | stage << 12,
            extended_flags: None,
            path: path.into(),
        }
    }

    fn index(mut entries: Vec<IndexEntry>) -> Index {
        entries.sort_by(|a, b| (&a.path, a.stage()).cmp(&(&b.path, b.stage())));
        Index {
            version: 2,
            entries,
            extensions: Vec::new(),
        }
    }

    /// What the index `after` has at each path where `before` differs.
    fn changes<'a>(before: &'a Index, after: &'a Index) -> Vec<Replaced<'a>> {
        let at = |index: &'a Index, path: &[u8]| -> Vec<&'a IndexEntry> {
            index.entries.iter().filter(|e| e.path == path).collect()
        };
        let mut paths: Vec<&[u8]> = before
            .entries
            .iter()
            .chain(&after.entries)
            .map(|e| e.path.as_slice())
            .collect();
        paths.sort();
        paths.dedup();
        let differ = paths
            .into_iter()
            .filter(|path| at(before, path) != at(after, path));
        differ
            .map(|path| Replaced {
                path: after
                    .entries
                    .iter()
                    .find(|e| e.path == path)
                    .map_or(path, |e| &e.path),
                entries: at(after, path)
                    .iter()
                    .map(|e| (e.mode, e.flags, e.oid))
                    .collect(),
            })
            .collect()
    }

    fn says(a: &Snapshot) -> impl PartialEq + std::fmt::Debug + '_ {
        let entry = |i: usize| {
            (
                a.path(i),
                &a.entries[i].oid,
                a.entries[i].mode,
                a.entries[i].flags,
                a.entries[i].stat,
            )
        };
        let dir = |d: usize| (a.dir_path(d), &a.dirs[d]);
        let entries: Vec<_> = (0..a.entries.len()).map(entry).collect();
        let dirs: Vec<_> = (0..a.dirs.len()).map(dir).collect();
        (&a.index_id, entries, dirs)
    }

    /// The snapshot of `before` with a stat for every entry and a tree for
    /// every directory that makes one, and beside it what both ways make
    /// of it for `after`.
    fn both(before: &Index, after: &Index) -> (Snapshot, Snapshot) {
        let mut prior = Snapshot::of_index(before, b"one", ALGO, None);
        for e in 0..prior.entries.len() {
            prior.set_stat(e, Some(stat(e as u64 + 1)));
        }
        prior.fill_trees();
        let whole = Snapshot::of_index(after, b"two", ALGO, Some(&prior));
        let mut replaced = prior.clone();
        // from a file and back, as a command after this one reads it
        let tmp = alt_os::temp::tempdir().unwrap();
        let path = tmp.path().join("index.scan");
        replaced.save(&path).unwrap();
        replaced.replace_paths(b"two", &changes(before, after));
        replaced.save(&path).unwrap();
        let read = Snapshot::load(&path, ALGO).unwrap();
        assert_eq!(says(&read), says(&replaced));
        (whole, replaced)
    }

    #[test]
    fn content_replaced_at_a_path_leaves_the_rest_as_it_was() {
        let files = |c: u8| {
            vec![
                entry("a", 0, 1),
                entry("b/c", 0, 2),
                entry("b/d/e", 0, c),
                entry("f/g", 0, 4),
            ]
        };
        let (whole, replaced) = both(&index(files(3)), &index(files(9)));
        assert_eq!(says(&replaced), says(&whole));
        let trees: Vec<bool> = replaced.dirs.iter().map(|d| d.tree.is_some()).collect();
        // the root, b and b/d lost theirs, f kept its
        assert_eq!(trees, vec![false, false, false, true]);
        let stats: Vec<bool> = replaced.entries.iter().map(|e| e.stat.is_some()).collect();
        assert_eq!(stats, vec![true, true, false, true]);
    }

    #[test]
    fn paths_that_come_and_go_and_conflicts_make_what_the_whole_index_makes() {
        let before = index(vec![
            entry("a", 0, 1),
            entry("b/c", 0, 2),
            entry("b/d/e", 0, 3),
            entry("f/g", 0, 4),
            entry("m", 1, 5),
            entry("m", 2, 6),
            entry("m", 3, 7),
            entry("z/y/x", 0, 8),
        ]);
        let cases = [
            // a new file in a new directory, one gone with its directory
            vec![
                entry("a", 0, 1),
                entry("b/c", 0, 2),
                entry("f/g", 0, 4),
                entry("h/i/j", 0, 9),
                entry("m", 1, 5),
                entry("m", 2, 6),
                entry("m", 3, 7),
                entry("z/y/x", 0, 8),
            ],
            // the conflict settled, and a path under a name that sorts beside a directory
            vec![
                entry("a", 0, 1),
                entry("b.c", 0, 9),
                entry("b/c", 0, 2),
                entry("b/d/e", 0, 3),
                entry("f/g", 0, 4),
                entry("m", 0, 6),
                entry("z/y/x", 0, 8),
            ],
            // a file where a directory was, a conflict where a file was
            vec![
                entry("a", 1, 1),
                entry("a", 3, 9),
                entry("b", 0, 2),
                entry("f/g", 0, 4),
                entry("m", 1, 5),
                entry("m", 2, 6),
                entry("m", 3, 7),
                entry("z/y/x", 0, 8),
            ],
            // one side of the conflict changed and nothing else
            vec![
                entry("a", 0, 1),
                entry("b/c", 0, 2),
                entry("b/d/e", 0, 3),
                entry("f/g", 0, 4),
                entry("m", 1, 5),
                entry("m", 2, 9),
                entry("m", 3, 7),
                entry("z/y/x", 0, 8),
            ],
            // content changed before the first path that comes or goes
            vec![
                entry("a", 0, 9),
                entry("b/c", 0, 2),
                entry("b/d/e", 0, 3),
                entry("b/d/f", 0, 3),
                entry("f/g", 0, 4),
                entry("m", 1, 5),
                entry("m", 2, 6),
                entry("m", 3, 7),
            ],
            vec![],
        ];
        for (n, after) in cases.into_iter().enumerate() {
            let after = index(after);
            let (whole, replaced) = both(&before, &after);
            assert_eq!(says(&replaced), says(&whole), "case {n}");
            let (whole, replaced) = both(&after, &before);
            assert_eq!(says(&replaced), says(&whole), "case {n} back");
        }
    }

    #[test]
    fn any_two_indexes_are_one_step_apart() {
        // a fixed sequence of numbers that looks shuffled
        let mut seed = 0x2545f4914f6cdd1du64;
        let mut next = |below: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % below
        };
        let names = [
            "a", "b", "c", "d/e", "d/f", "d/g/h", "d/g/i", "j/k", "j.l", "m/n/o/p", "q",
        ];
        let mut make = || {
            let mut entries = Vec::new();
            for name in names {
                match next(6) {
                    0 => {}
                    1 => {
                        for stage in 1..=3 {
                            if next(3) > 0 {
                                entries.push(entry(name, stage, next(4) as u8));
                            }
                        }
                    }
                    _ => entries.push(entry(name, 0, next(4) as u8)),
                }
            }
            index(entries)
        };
        for round in 0..300 {
            let (before, after) = (make(), make());
            let (whole, replaced) = both(&before, &after);
            assert_eq!(says(&replaced), says(&whole), "round {round}");
        }
    }
}
