//! A working tree watched for changes, for a process that scans it many
//! times: after one full scan, only the paths the system reported events
//! for are looked at again. Whenever the events cannot be trusted to be
//! complete, the tree is scanned in full.

use std::io;
use std::path::Path;

use crate::delta::{Delta, ScanOptions};
use crate::snapshot::{FileStat, Snapshot};
use crate::untracked::Untracked;

mod dirs;
mod mounts;
#[path = "watch/linux.rs"]
mod os;

/// More changed paths than this are answered by a full scan.
const MANY_PATHS: usize = 256;

/// What happened in the tree since the watcher was last asked.
pub(crate) enum Changes {
    /// these paths, relative to the root, each with everything beneath it
    Paths(Vec<Vec<u8>>),
    /// events were lost or the tree was rearranged: anything may differ
    All,
}

/// The most watches one tree may take: half of what the system allows the
/// user, which every tree and every other program of theirs draws on.
pub fn watch_allowance() -> io::Result<usize> {
    let all = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")?;
    let all: usize = all.trim().parse().map_err(io::Error::other)?;
    Ok(all / 2)
}

struct Cached {
    index_id: Vec<u8>,
    /// the files outside the tree that decide what a scan answers, as
    /// they were
    outside: Vec<Option<FileStat>>,
    delta: Delta,
}

pub struct Watched {
    watcher: os::Watcher,
    /// the index the watcher last chose its directories by
    index_id: Vec<u8>,
    cached: Option<Cached>,
    /// what was taken off the queue between two scans
    absorbed: Option<Changes>,
    /// the queue could not be read: the tree is no longer watched
    failed: bool,
}

impl Watched {
    /// Starts watching the tree at `root`, which takes one watch for every
    /// directory a scan of it for `snapshot` looks into; a tree with more
    /// than `most` of them is not watched, and fails the way a tree does
    /// that the system has no watches left for. Fails as well for a tree
    /// that events do not tell everything about. The first scan sees the
    /// whole tree, and it runs after every directory is being watched.
    pub fn start(root: &Path, most: usize, snapshot: &Snapshot) -> io::Result<Watched> {
        Ok(Watched {
            watcher: os::Watcher::start(root, most, &|dir| tracked(snapshot, dir))?,
            index_id: snapshot.index_id.clone(),
            cached: None,
            absorbed: None,
            failed: false,
        })
    }

    /// How often the watches were set up from the root: once at the start,
    /// and again each time events were lost.
    pub fn set_up(&self) -> usize {
        self.watcher.set_up
    }

    /// Takes the events queued so far off the queue, to be answered for by
    /// the next scan. For a process that writes many files to the tree
    /// itself, more than the queue holds: called every so often while it
    /// does, it keeps the queue from running over.
    pub fn absorb(&mut self) {
        // the index is being replaced: every new directory is watched
        match self.watcher.changes(&|_| true) {
            Ok(more) => self.absorbed = Some(merged(self.absorbed.take(), more)),
            Err(_) => self.failed = true,
        }
    }

    /// What happened in the tree since this was last asked, the watches
    /// brought in line with the index of `snapshot` first.
    fn changes(&mut self, snapshot: &Snapshot) -> Option<Changes> {
        let tracked = |dir: &[u8]| tracked(snapshot, dir);
        if self.index_id != snapshot.index_id {
            self.watcher.revisit(&tracked).ok()?;
            self.index_id.clone_from(&snapshot.index_id);
        }
        let fresh = self.watcher.changes(&tracked).ok()?;
        Some(merged(self.absorbed.take(), fresh))
    }

    /// What [`scan`](crate::scan) of the whole tree returns for `opts`,
    /// which ask for no untracked files or for directories as one path.
    /// `settings` are the files outside the tree that `opts` read what
    /// they do with a file from: a change in one of them is answered by a
    /// scan of everything. `None` when the tree can no longer be watched
    /// or a scan of it failed: the caller scans it itself, which answers
    /// or fails as it does for a tree that is not watched, and drops this
    /// value.
    pub fn scan(
        &mut self,
        root: &Path,
        snapshot: &mut Snapshot,
        opts: &ScanOptions<'_>,
        settings: &[&Path],
    ) -> Option<Delta> {
        if self.failed {
            return None;
        }
        // before anything is looked at: an event that comes later is seen next time
        let changes = self.changes(snapshot)?;
        let outside = stats(settings);
        // taken out: a scan that fails leaves nothing to build on
        let cached = self.cached.take();
        let whole = |within| ScanOptions {
            untracked: Untracked::Normal,
            within,
            clean: opts.clean,
            head: opts.head,
            clock: opts.clock,
        };
        let dirty = match changes {
            Changes::Paths(paths) => Some(dirty_paths(paths, snapshot)),
            Changes::All => None,
        };
        let delta = match (dirty, cached) {
            (Some(dirty), Some(cached))
                if cached.index_id == snapshot.index_id
                    && cached.outside == outside
                    && dirty.len() <= MANY_PATHS
                    && dirty.iter().all(|p| !p.is_empty()) =>
            {
                let mut delta = cached.delta;
                if !dirty.is_empty() {
                    let within: Vec<&[u8]> = dirty.iter().map(Vec::as_slice).collect();
                    let fresh = crate::scan(root, snapshot, &whole(&within)).ok()?;
                    delta.replace_within(&within, fresh);
                }
                delta
            }
            _ => crate::scan(root, snapshot, &whole(&[])).ok()?,
        };
        self.cached = Some(Cached {
            index_id: snapshot.index_id.clone(),
            outside,
            delta: delta.clone(),
        });
        let mut delta = delta;
        if opts.untracked == Untracked::No {
            delta.untracked.clear();
        }
        Some(delta)
    }
}

/// Whether the index has an entry beneath `dir`.
fn tracked_in(snapshot: &Snapshot, dir: &[u8]) -> bool {
    // the index is ordered by path: the entries beneath `dir` begin where
    // `dir/` would stand
    let key = [dir, b"/"].concat();
    let first = partition(snapshot, |entry| entry < key.as_slice());
    first < snapshot.entries.len() && beneath(snapshot.path(first), dir)
}

/// Whether the index has an entry beneath `dir` or at it, as a submodule is.
fn tracked(snapshot: &Snapshot, dir: &[u8]) -> bool {
    tracked_in(snapshot, dir) || snapshot.find(dir).is_some()
}

/// What `earlier` and `later` tell together; more paths than a scan of
/// them is worth stand for the whole tree.
fn merged(earlier: Option<Changes>, later: Changes) -> Changes {
    match (earlier, later) {
        (None, later) => later,
        (Some(Changes::Paths(mut paths)), Changes::Paths(more)) => {
            paths.extend(more);
            match paths.len() > 4 * MANY_PATHS {
                true => Changes::All,
                false => Changes::Paths(paths),
            }
        }
        _ => Changes::All,
    }
}

/// What each of the files looks like now; `None` for one that is not there.
fn stats(files: &[&Path]) -> Vec<Option<FileStat>> {
    let stat = |path: &&Path| std::fs::metadata(path).ok().map(|m| FileStat::of(&m));
    files.iter().map(stat).collect()
}

fn beneath(path: &[u8], dir: &[u8]) -> bool {
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
fn dirty_paths(paths: Vec<Vec<u8>>, snapshot: &Snapshot) -> Vec<Vec<u8>> {
    let gitlinks: Vec<&[u8]> = (0..snapshot.entries.len())
        .filter(|&i| snapshot.entries[i].mode == 0o160000)
        .map(|i| snapshot.path(i))
        .collect();
    let mut dirty: Vec<Vec<u8>> = paths
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
        .map(|path| outermost_untracked(path, snapshot))
        .collect();
    // a submodule can move to another commit without an event in this tree
    dirty.extend(gitlinks.iter().map(|g| g.to_vec()));
    dirty.sort();
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
/// at from the outermost such directory.
fn outermost_untracked(path: Vec<u8>, snapshot: &Snapshot) -> Vec<u8> {
    let slashes = path.iter().enumerate().filter(|(_, b)| **b == b'/');
    for (at, _) in slashes {
        if !tracked_in(snapshot, &path[..at]) {
            return path[..at].to_vec();
        }
    }
    path
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
mod tests {
    use super::*;
    use alt_git_codec::HashAlgo;
    use alt_git_index::Index;

    /// A snapshot of an index with a file at each of `paths`.
    fn tracking(paths: &[&str]) -> Snapshot {
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
    }
}
