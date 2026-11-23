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

mod found;
mod listing;
mod reach;
pub(crate) use found::Found;
pub(crate) use reach::Reach;
use reach::Within;

pub(crate) struct Walk<'a> {
    snapshot: &'a Snapshot,
    /// the paths to look at; all when there are none
    within: Within<'a>,
    pub(crate) untracked: Untracked,
    /// whether a file can be executed is told from its stat
    file_mode: bool,
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
    opts: &crate::ScanOptions<'_>,
    ignore: &IgnoreStack,
) -> io::Result<Found> {
    let (untracked, within) = (opts.untracked, opts.within);
    let walk = Walk {
        snapshot,
        within: Within::new(within),
        untracked,
        file_mode: opts.file_mode,
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

impl Walk<'_> {
    pub(crate) fn reach(&self, parent: Reach, path: &[u8]) -> Option<Reach> {
        self.within.reach(parent, path)
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
        } else if entry.stat != Some(stat) || self.other_mode(entry.mode, stat) {
            found.suspects.push((first as u32, stat));
        }
    }

    /// Whether the file with `stat` is to have its mode looked at though it
    /// shows the stat recorded for it: the stat was recorded while the
    /// mode of a file was not told from its stat, and tells another mode
    /// than the index has.
    fn other_mode(&self, staged: u32, stat: FileStat) -> bool {
        self.file_mode && staged & 0o170000 == 0o100000 && stat.git_mode() != staged
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
