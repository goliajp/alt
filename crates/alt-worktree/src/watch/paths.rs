//! Which paths of a watched tree a scan looks at again for the events
//! reported, and what the index says about a directory.

use crate::snapshot::Snapshot;

/// Whether the index has an entry beneath `dir`.
fn tracked_in(snapshot: &Snapshot, dir: &[u8]) -> bool {
    // the index is ordered by path: the entries beneath `dir` begin where
    // `dir/` would stand
    let key = [dir, b"/"].concat();
    let first = partition(snapshot, |entry| entry < key.as_slice());
    first < snapshot.entries.len() && beneath(snapshot.path(first), dir)
}

/// Whether the index has an entry beneath `dir` or at it, as a submodule is.
pub(super) fn tracked(snapshot: &Snapshot, dir: &[u8]) -> bool {
    tracked_in(snapshot, dir) || snapshot.find(dir).is_some()
}

pub(super) fn beneath(path: &[u8], dir: &[u8]) -> bool {
    path.len() > dir.len() && path[dir.len()] == b'/' && path.starts_with(dir)
}

fn parent(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|b| *b == b'/') {
        Some(at) => &path[..at],
        None => b"",
    }
}

/// The paths to look at again for the reported `paths`; the empty path
/// stands for the whole tree.
pub(super) fn dirty_paths(paths: Vec<Vec<u8>>, snapshot: &Snapshot) -> Vec<Vec<u8>> {
    let gitlinks: Vec<&[u8]> = (0..snapshot.entries.len())
        .filter(|&i| snapshot.entries[i].mode == 0o160000)
        .map(|i| snapshot.path(i))
        .collect();
    let mut named: Vec<Vec<u8>> = paths
        .into_iter()
        .map(|path| {
            let name = &path[parent(&path).len()..];
            let name = name.strip_prefix(b"/").unwrap_or(name);
            // ignore rules and attributes reach everything beneath their
            // directory, and a repository in a directory changes what
            // counts as its files
            if matches!(name, b".gitignore" | b".gitattributes" | b".git" | b".alt") {
                return parent(&path).to_vec();
            }
            // what a submodule has checked out is asked of the submodule itself
            match gitlinks.iter().find(|g| beneath(&path, g)) {
                Some(gitlink) => gitlink.to_vec(),
                None => path,
            }
        })
        .collect();
    // neighbours one after the other: what is known of a directory is
    // known for the next path in it
    named.sort_unstable();
    named.dedup();
    let mut lifted = Lifted::new(snapshot);
    let mut dirty: Vec<Vec<u8>> = named.into_iter().map(|path| lifted.of(path)).collect();
    // a submodule can move to another commit without an event in this tree
    dirty.extend(gitlinks.iter().map(|g| g.to_vec()));
    dirty.sort_unstable();
    dirty.dedup();
    let mut kept: Vec<Vec<u8>> = Vec::new();
    for path in dirty {
        if !kept
            .last()
            .is_some_and(|k| k.is_empty() || beneath(&path, k))
        {
            kept.push(path);
        }
    }
    kept
}

/// A directory with nothing tracked in it is reported as one path, which
/// stands or falls with any file beneath it: a change in there is looked
/// at from the outermost such directory. Asked for one path after the
/// other, this looks a directory up in the index once for all the paths
/// that follow one another in it.
pub(super) struct Lifted<'a> {
    snapshot: &'a Snapshot,
    /// the path last looked up
    last: Vec<u8>,
    /// how much of it is the outermost directory with nothing tracked in
    /// it; every directory above has something
    cut: Option<usize>,
}

impl<'a> Lifted<'a> {
    pub(super) fn new(snapshot: &'a Snapshot) -> Lifted<'a> {
        Lifted {
            snapshot,
            last: Vec::new(),
            cut: None,
        }
    }

    /// `path`, or the outermost directory above it with nothing tracked
    /// in it.
    pub(super) fn of(&mut self, path: Vec<u8>) -> Vec<u8> {
        let same = self.last.iter().zip(&path).take_while(|(a, b)| a == b);
        let same = same.count();
        if let Some(cut) = self.cut.filter(|&cut| cut < same) {
            return path[..cut].to_vec();
        }
        // the directories both paths lie in are known to have something
        let cut = (same..path.len())
            .filter(|&at| path[at] == b'/')
            .find(|&at| !tracked_in(self.snapshot, &path[..at]));
        let lifted = match cut {
            Some(cut) => path[..cut].to_vec(),
            None => path.clone(),
        };
        self.last = path;
        self.cut = cut;
        lifted
    }
}

#[cfg(test)]
fn outermost_untracked(path: Vec<u8>, snapshot: &Snapshot) -> Vec<u8> {
    Lifted::new(snapshot).of(path)
}

/// The first entry of the snapshot whose path `before` does not hold for.
fn partition(snapshot: &Snapshot, before: impl Fn(&[u8]) -> bool) -> usize {
    let (mut low, mut high) = (0, snapshot.entries.len());
    while low < high {
        let mid = low + (high - low) / 2;
        match before(snapshot.path(mid)) {
            true => low = mid + 1,
            false => high = mid,
        }
    }
    low
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use alt_git_codec::HashAlgo;
    use alt_git_index::Index;

    /// A snapshot of an index with a file at each of `paths`.
    pub(in crate::watch) fn tracking(paths: &[&str]) -> Snapshot {
        let entry = |path: &&str| alt_git_index::IndexEntry {
            ctime: (0, 0),
            mtime: (0, 0),
            dev: 0,
            ino: 0,
            mode: 0o100644,
            uid: 0,
            gid: 0,
            size: 0,
            oid: alt_git_codec::ObjectId::hash_object(
                HashAlgo::Sha1,
                alt_git_codec::ObjectKind::Blob,
                b"",
            ),
            flags: path.len() as u16,
            extended_flags: None,
            path: (*path).into(),
        };
        let index = Index {
            version: 2,
            entries: paths.iter().map(entry).collect(),
            extensions: Vec::new(),
        };
        Snapshot::of_index(&index, b"", HashAlgo::Sha1, None)
    }

    #[test]
    fn dirty_paths_cover_ignore_files_and_drop_what_lies_beneath_another() {
        let snapshot = tracking(&["a/b/t", "a/bc/t", "a/x/t", "b/t", "t"]);
        let paths = |given: &[&str]| -> Vec<String> {
            let given = given.iter().map(|p| p.as_bytes().to_vec()).collect();
            let dirty = dirty_paths(given, &snapshot);
            dirty
                .into_iter()
                .map(|p| String::from_utf8(p).unwrap())
                .collect()
        };
        assert_eq!(
            paths(&["a/b/c", "a/b", "a/bc", "z", "z"]),
            ["a/b", "a/bc", "z"]
        );
        assert_eq!(paths(&["a/.gitignore", "a/x/y", "b/.git"]), ["a", "b"]);
        assert_eq!(paths(&["a/b/.gitattributes", "a/b/c"]), ["a/b"]);
        assert_eq!(paths(&["x", ".gitignore", "a/b"]), [""]);
        // in directories with nothing tracked: the outermost of them
        assert_eq!(
            paths(&["new/deep/file", "new/other", "a/new/.gitignore"]),
            ["a/new", "new"]
        );
        assert!(paths(&[]).is_empty());
    }

    #[test]
    fn a_path_in_an_untracked_directory_stands_for_the_outermost_one() {
        let snapshot = tracking(&[
            "a.txt",
            "a/b/c.txt",
            "a0",
            "src-old/x",
            "src.rs",
            "src/lib.rs",
        ]);
        let lifted = |path: &str| {
            String::from_utf8(outermost_untracked(path.as_bytes().to_vec(), &snapshot)).unwrap()
        };
        // in tracked directories all the way: the path itself
        assert_eq!(lifted("top.txt"), "top.txt");
        assert_eq!(lifted("a/new.txt"), "a/new.txt");
        assert_eq!(lifted("a/b/c.txt"), "a/b/c.txt");
        assert_eq!(lifted("src/new"), "src/new");
        // the first directory on the way that has nothing tracked in it
        assert_eq!(lifted("new/deep/er/file"), "new");
        assert_eq!(lifted("a/new/deep/file"), "a/new");
        assert_eq!(lifted("a/b/x/y"), "a/b/x");
        // a tracked file is no tracked directory, nor is a name that only
        // begins like one
        assert_eq!(lifted("a.txt/x/y"), "a.txt");
        assert_eq!(lifted("a0/x"), "a0");
        assert_eq!(lifted("sr/x"), "sr");
        assert_eq!(lifted("src.rs/x"), "src.rs");
        assert_eq!(lifted("src-/x"), "src-");
        // one after the other, in any order, each as it is by itself
        let paths = [
            "a/b/c.txt",
            "a/b/x/y",
            "a/b/x/z/w",
            "a/b/y",
            "a/new/deep/file",
            "a/new/other",
            "a/n",
            "a.txt/x/y",
            "a0/x",
            "new/deep/er/file",
            "new/deep/file",
            "new",
            "sr/x",
            "src/new",
            "src/new/x",
            "src.rs/x",
            "top.txt",
            "a/b/x",
            "a/b",
            "a",
        ];
        let mut one_after_the_other = Lifted::new(&snapshot);
        for path in paths.iter().chain(paths.iter().rev()) {
            let got = one_after_the_other.of(path.as_bytes().to_vec());
            assert_eq!(String::from_utf8(got).unwrap(), lifted(path), "{path}");
        }
    }
}
