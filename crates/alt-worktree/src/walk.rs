//! Walks the working tree beside a snapshot of the index: each directory is
//! opened once, its names are matched against the index entries beneath
//! it, and only what does not show the recorded stat is reported.

use std::ffi::CString;
use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::fsdir::{DirFd, Kind, Listing, kind_of};
use crate::ignore::{self, IgnoreStack};
use crate::snapshot::{FileStat, Snapshot};
use crate::untracked::Untracked;

/// Directories with fewer entries beneath them are walked by one thread.
const SPLIT: u32 = 512;

/// A directory with fewer files of its own has them looked at by one thread.
const MANY_FILES: usize = 256;

/// What a walk found to differ from the snapshot. Entries are named by
/// their position in it.
#[derive(Default)]
pub(crate) struct Found {
    /// a file is there, with another stat than the one recorded
    pub suspects: Vec<(u32, FileStat)>,
    /// nothing is there, or a directory
    pub deleted: Vec<u32>,
    /// files the index does not have and no ignore rule hides
    pub untracked: Vec<Vec<u8>>,
    /// submodule entries with a directory in place
    pub gitlinks: Vec<u32>,
    /// conflicted paths with a file in place
    pub unmerged: Vec<u32>,
}

impl Found {
    fn absorb(&mut self, other: Found) {
        self.suspects.extend(other.suspects);
        self.deleted.extend(other.deleted);
        self.untracked.extend(other.untracked);
        self.gitlinks.extend(other.gitlinks);
        self.unmerged.extend(other.unmerged);
    }
}

pub(crate) struct Walk<'a> {
    snapshot: &'a Snapshot,
    /// the paths to look at, each with everything beneath it; all when empty
    within: &'a [&'a [u8]],
    pub(crate) untracked: Untracked,
}

/// How a path stands to the paths a walk is limited to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    /// one of them, or beneath one
    Inside,
    /// a directory above one
    Above,
}

/// A directory in the one being walked, which takes a walk of its own:
/// its place in the listing, and its index directory if it has one.
#[derive(Clone, Copy)]
struct Below {
    item: usize,
    tracked: Option<(usize, bool)>,
    reach: Reach,
}

/// Walks the tree at `root`. Where untracked files are asked for,
/// directories are listed and what the index does not have is reported;
/// where not, each index entry is looked up by name and nothing else is
/// seen.
pub(crate) fn walk(
    root: &Path,
    snapshot: &Snapshot,
    untracked: Untracked,
    within: &[&[u8]],
    ignore: &IgnoreStack,
) -> io::Result<Found> {
    let walk = Walk {
        snapshot,
        within,
        untracked,
    };
    let reach = match within.is_empty() {
        true => Reach::Inside,
        false => Reach::Above,
    };
    let mut found = Found::default();
    let dir = DirFd::open(root)?;
    match untracked {
        Untracked::No => walk.unlisted(&dir, 0, reach, &mut found)?,
        _ => walk.listed(dir, 0, ignore, false, reach, &mut found)?,
    }
    Ok(found)
}

/// A child of an index directory: a path with its entries, one per stage,
/// or a directory.
enum Child {
    File(usize, usize),
    Dir(usize),
}

fn beneath(path: &[u8], dir: &[u8]) -> bool {
    path.len() > dir.len() && path[dir.len()] == b'/' && path.starts_with(dir)
}

impl Walk<'_> {
    pub(crate) fn reach(&self, parent: Reach, path: &[u8]) -> Option<Reach> {
        if parent == Reach::Inside || self.within.iter().any(|w| *w == path || beneath(path, w)) {
            return Some(Reach::Inside);
        }
        self.within
            .iter()
            .any(|w| beneath(w, path))
            .then_some(Reach::Above)
    }

    fn children(&self, dir: usize) -> impl Iterator<Item = Child> + '_ {
        let s = self.snapshot;
        let end = s.dirs[dir].end as usize;
        let mut at = s.dirs[dir].first as usize;
        let mut dirs = s.child_dirs(dir).peekable();
        std::iter::from_fn(move || {
            if at >= end {
                return None;
            }
            if let Some(&child) = dirs.peek()
                && s.dirs[child].first as usize == at
            {
                dirs.next();
                at = s.dirs[child].end as usize;
                return Some(Child::Dir(child));
            }
            let first = at;
            at += 1;
            while at < end && s.path(at) == s.path(first) {
                at += 1;
            }
            Some(Child::File(first, at))
        })
    }

    fn path(&self, child: &Child) -> &[u8] {
        match *child {
            Child::File(first, _) => self.snapshot.path(first),
            Child::Dir(c) => self.snapshot.dir_path(c),
        }
    }

    /// The last component of a child's path within `dir`.
    fn name(&self, dir: usize, child: &Child) -> &[u8] {
        let skip = match self.snapshot.dirs[dir].path_len {
            0 => 0,
            len => len as usize + 1,
        };
        &self.path(child)[skip..]
    }

    /// A tracked path with something other than a directory in place.
    fn tracked(&self, first: usize, end: usize, stat: FileStat, found: &mut Found) {
        let entry = &self.snapshot.entries[first];
        if end - first > 1 || entry.stage() != 0 {
            found.unmerged.push(first as u32);
        } else if kind_of(stat.mode) == Kind::Other {
            found.deleted.push(first as u32);
        } else if entry.stat != Some(stat) {
            found.suspects.push((first as u32, stat));
        }
    }

    /// The entries `first..end` have nothing in place.
    fn gone(&self, first: usize, end: usize, reach: Reach, found: &mut Found) {
        let s = self.snapshot;
        // a submodule too: one that is not checked out still has its directory
        found.deleted.extend(
            (first..end)
                .filter(|&i| self.reach(reach, s.path(i)) == Some(Reach::Inside))
                .map(|i| i as u32),
        );
    }

    fn all_gone(&self, dir: usize, reach: Reach, found: &mut Found) {
        let d = &self.snapshot.dirs[dir];
        self.gone(d.first as usize, d.end as usize, reach, found);
    }

    /// Walks an index directory without listing it: one stat per entry.
    fn unlisted(&self, fd: &DirFd, dir: usize, reach: Reach, found: &mut Found) -> io::Result<()> {
        let mut below = Vec::new();
        let mut files = Vec::new();
        for child in self.children(dir) {
            let Some(reach) = self.reach(reach, self.path(&child)) else {
                continue;
            };
            let name = CString::new(self.name(dir, &child)).map_err(io::Error::other)?;
            match child {
                Child::File(..) if reach != Reach::Inside => {}
                Child::File(first, end) => files.push((first, end, name)),
                Child::Dir(c) => below.push((c, name, reach)),
            }
        }
        let stat = |(first, end, name): &(usize, usize, CString), found: &mut Found| {
            let (first, end) = (*first, *end);
            match fd.stat(name)? {
                None => self.gone(first, end, Reach::Inside, found),
                Some(st) if kind_of(st.mode) == Kind::Dir => {
                    match self.snapshot.entries[first].mode == 0o160000 {
                        true => found.gitlinks.push(first as u32),
                        false => self.gone(first, end, Reach::Inside, found),
                    }
                }
                Some(st) => self.tracked(first, end, st, found),
            }
            Ok(())
        };
        self.files(&files, stat, found)?;
        let visit = |(c, name, reach): &(usize, CString, Reach)| -> io::Result<Found> {
            let mut found = Found::default();
            match fd.open_at(name)? {
                Some(sub) => self.unlisted(&sub, *c, *reach, &mut found)?,
                None => self.all_gone(*c, *reach, &mut found),
            }
            Ok(found)
        };
        self.below(dir, &below, visit, found)
    }

    /// Looks at the files of one directory with `stat`, on several threads
    /// when there are many.
    fn files<T: Sync>(
        &self,
        files: &[T],
        stat: impl Fn(&T, &mut Found) -> io::Result<()> + Sync,
        found: &mut Found,
    ) -> io::Result<()> {
        if files.len() < MANY_FILES {
            return files.iter().try_for_each(|file| stat(file, found));
        }
        let parts: Vec<&[T]> = files.chunks(MANY_FILES / 4).collect();
        let each = |part: &&[T]| -> io::Result<Found> {
            let mut found = Found::default();
            part.iter().try_for_each(|file| stat(file, &mut found))?;
            Ok(found)
        };
        for part in alt_threads::map(&parts, 1, each) {
            found.absorb(part?);
        }
        Ok(())
    }

    /// Runs `visit` over what lies below a directory, on several threads
    /// when the directory is large enough to be worth it.
    fn below<T: Sync>(
        &self,
        dir: usize,
        items: &[T],
        visit: impl Fn(&T) -> io::Result<Found> + Sync,
        found: &mut Found,
    ) -> io::Result<()> {
        let d = &self.snapshot.dirs[dir];
        let results = match d.end - d.first >= SPLIT && items.len() > 1 {
            true => alt_threads::map(items, 1, visit),
            false => items.iter().map(visit).collect(),
        };
        for result in results {
            found.absorb(result?);
        }
        Ok(())
    }

    /// Walks an index directory by its listing, which also shows what the
    /// index does not have. Inside an ignored directory (`hidden`) only the
    /// tracked files count.
    fn listed(
        &self,
        mut fd: DirFd,
        dir: usize,
        ignore: &IgnoreStack,
        hidden: bool,
        reach: Reach,
        found: &mut Found,
    ) -> io::Result<()> {
        let mut names = fd.names()?;
        names.sort();
        let base = self.snapshot.dir_path(dir);
        let own = match hidden {
            true => None,
            false => ignore_file(&fd, &names, base, ignore)?,
        };
        let ignore = own.as_ref().unwrap_or(ignore);
        let mut taken = vec![false; names.len()];
        let mut below = Vec::new();
        let mut files = Vec::new();
        for child in self.children(dir) {
            let Some(reach) = self.reach(reach, self.path(&child)) else {
                continue;
            };
            let name = self.name(dir, &child);
            match child {
                Child::File(..) if reach != Reach::Inside => {}
                Child::File(first, end) => {
                    let gitlink = self.snapshot.entries[first].mode == 0o160000;
                    if gitlink && let Some(item) = names.find(name, true) {
                        taken[item] = true;
                        found.gitlinks.push(first as u32);
                        continue;
                    }
                    let Some(item) = names.find(name, false) else {
                        self.gone(first, end, reach, found);
                        continue;
                    };
                    taken[item] = true;
                    files.push((first, end, item));
                }
                Child::Dir(c) => match names.find(name, true) {
                    Some(item) => {
                        taken[item] = true;
                        let hidden = hidden || ignore.is_ignored(self.snapshot.dir_path(c), true);
                        below.push(Below {
                            item,
                            tracked: Some((c, hidden)),
                            reach,
                        });
                    }
                    None => self.all_gone(c, reach, found),
                },
            }
        }
        let stat = |&(first, end, item): &(usize, usize, usize), found: &mut Found| {
            match fd.stat(names.c_name(item))? {
                Some(st) if kind_of(st.mode) != Kind::Dir => self.tracked(first, end, st, found),
                _ => self.gone(first, end, Reach::Inside, found),
            }
            Ok(())
        };
        self.files(&files, stat, found)?;
        if !hidden {
            let mut path = base.to_vec();
            for item in (0..names.len()).filter(|&i| !taken[i]) {
                let name = names.name(item);
                if name == b".alt" || name == b".git" {
                    continue;
                }
                let is_dir = names.kind(item) == Kind::Dir;
                with_name(&mut path, name, |path| match self.reach(reach, path) {
                    Some(reach) if is_dir && !ignore.is_ignored(path, true) => below.push(Below {
                        item,
                        tracked: None,
                        reach,
                    }),
                    Some(Reach::Inside) if !is_dir && !ignore.is_ignored(path, false) => {
                        found.untracked.push(path.to_vec())
                    }
                    _ => {}
                });
            }
        }
        let visit = |below: &Below| -> io::Result<Found> {
            let mut found = Found::default();
            let sub = fd.open_at(names.c_name(below.item))?;
            match (below.tracked, sub) {
                (Some((dir, hidden)), Some(sub)) => {
                    self.listed(sub, dir, ignore, hidden, below.reach, &mut found)?
                }
                (Some((dir, _)), None) => self.all_gone(dir, below.reach, &mut found),
                (None, Some(sub)) => {
                    let mut path = base.to_vec();
                    if !path.is_empty() {
                        path.push(b'/');
                    }
                    path.extend_from_slice(names.name(below.item));
                    self.untracked_in(sub, &mut path, ignore, below.reach, &mut found.untracked)?;
                }
                (None, None) => {}
            }
            Ok(found)
        };
        self.below(dir, &below, visit, found)
    }
}

pub(crate) fn holds_repository(names: &Listing) -> bool {
    names
        .find(b".git", false)
        .or(names.find(b".git", true))
        .is_some()
}

/// Calls `f` with `path` extended by `name`, then puts `path` back.
fn with_name(path: &mut Vec<u8>, name: &[u8], f: impl FnOnce(&[u8])) {
    let mark = path.len();
    if mark > 0 {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    f(path);
    path.truncate(mark);
}

/// The ignore rules with this directory's `.gitignore` on top, when it has one.
pub(crate) fn ignore_file(
    fd: &DirFd,
    names: &Listing,
    base: &[u8],
    ignore: &IgnoreStack,
) -> io::Result<Option<IgnoreStack>> {
    // a symlink is not followed, as git does not follow one in the tree
    let Some(item) = names
        .find(b".gitignore", false)
        .filter(|&item| names.kind(item) == Kind::File)
    else {
        return Ok(None);
    };
    let content = fd.read(names.c_name(item))?;
    Ok(Some(
        ignore.with(Arc::new(ignore::parse_layer(&content, base))),
    ))
}
