//! The walk of a directory by its listing: what the index has in it is
//! matched against the names found, and the rest is untracked.

use std::io;

use super::{Below, Child, Found, Reach, Walk, with_name};
use crate::fsdir::{DirFd, Kind, Listing, kind_of};
use crate::ignore::IgnoreStack;
use crate::untracked::Untracked;

/// What a listed directory holds, as the walk of it sorts it out.
struct Here {
    /// which names of the listing the index has
    taken: Vec<bool>,
    /// the directories to walk
    below: Vec<Below>,
    /// the tracked files to look at: their entries and their name
    files: Vec<(usize, usize, usize)>,
    /// index directories that are, to the file system, a directory of
    /// `below` under another spelling: the name both have, and the directory
    twins: Vec<(usize, usize)>,
}

impl Walk<'_> {
    /// Walks an index directory by its listing, which also shows what the
    /// index does not have. Inside an ignored directory (`hidden`) only the
    /// tracked files count. The index directories `twins` are this same
    /// directory under other spellings, and what they hold is tracked in
    /// it as well.
    #[expect(clippy::too_many_arguments, reason = "the state of one walk")]
    pub(super) fn listed(
        &self,
        mut fd: DirFd,
        dir: usize,
        twins: &[usize],
        ignore: &IgnoreStack,
        hidden: bool,
        reach: Reach,
        found: &mut Found,
    ) -> io::Result<()> {
        let mut names = fd.names(self.precompose)?;
        names.sort();
        let base = self.snapshot.dir_path(dir);
        if !hidden {
            found.listed.push(dir as u32);
            found.listed.extend(twins.iter().map(|&twin| twin as u32));
        }
        let own = match hidden {
            true => None,
            false => self.rules_in(&fd, &names, base, ignore)?,
        };
        let ignore = own.as_ref().unwrap_or(ignore);
        let mut here = Here {
            taken: vec![false; names.len()],
            below: Vec::new(),
            files: Vec::new(),
            twins: Vec::new(),
        };
        for &dir in std::iter::once(&dir).chain(twins) {
            self.tracked_in(&fd, dir, &names, ignore, hidden, reach, &mut here, found)?;
        }
        let stat = |&(first, end, item): &(usize, usize, usize), found: &mut Found| {
            match fd.stat(names.c_name(item))? {
                Some(st) if kind_of(st.mode) != Kind::Dir => self.tracked(first, end, st, found),
                _ => self.gone(first, end, Reach::Inside, found),
            }
            Ok(())
        };
        self.files(&here.files, stat, found)?;
        if !hidden {
            self.untracked_of(base, &names, ignore, reach, &mut here, found);
        }
        let visit = |below: &Below| {
            let twins = here.twins.iter().filter(|(item, _)| *item == below.item);
            let twins: Vec<usize> = twins.map(|&(_, twin)| twin).collect();
            self.visit(&fd, &names, base, ignore, below, &twins)
        };
        self.below(dir, &here.below, visit, found)
    }

    /// Matches the children the index has in the directory `dir` against
    /// its listing `names`: files to look at and directories to walk go to
    /// `here`, what has nothing in place to `found`.
    #[expect(clippy::too_many_arguments, reason = "one step of `listed`")]
    fn tracked_in(
        &self,
        fd: &DirFd,
        dir: usize,
        names: &Listing,
        ignore: &IgnoreStack,
        hidden: bool,
        reach: Reach,
        here: &mut Here,
        found: &mut Found,
    ) -> io::Result<()> {
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
                        here.taken[item] = true;
                        found.gitlinks.push(first as u32);
                        continue;
                    }
                    let item = match names.find(name, false) {
                        None => self.spelled(fd, names, name, false)?,
                        item => item,
                    };
                    let Some(item) = item else {
                        self.no_file(names, name, (first, end), reach, here, found);
                        continue;
                    };
                    here.taken[item] = true;
                    here.files.push((first, end, item));
                }
                Child::Dir(c) => {
                    let item = match names.find(name, true) {
                        None => self.spelled(fd, names, name, true)?,
                        item => item,
                    };
                    let Some(item) = item else {
                        self.all_gone(c, reach, found);
                        continue;
                    };
                    // walked once, for the directory of the index that
                    // came to it first
                    let walked = |below: &Below| below.item == item && below.tracked.is_some();
                    if here.below.iter().any(walked) {
                        here.twins.push((item, c));
                        continue;
                    }
                    here.taken[item] = true;
                    let inside = hidden;
                    let hidden = hidden || ignore.is_ignored(self.snapshot.dir_path(c), true);
                    if hidden && !inside {
                        found.unwalked.push(self.snapshot.dir_path(c).to_vec());
                    }
                    here.below.push(Below {
                        item,
                        tracked: Some((c, hidden)),
                        reach,
                    });
                }
            }
        }
        Ok(())
    }

    /// Where the file system takes names that differ in case for one:
    /// the name the listing has for what `name` reaches there, a
    /// directory or not as `dir` says. The file system tells which it is,
    /// by the file the two names reach.
    fn spelled(
        &self,
        fd: &DirFd,
        names: &Listing,
        name: &[u8],
        dir: bool,
    ) -> io::Result<Option<usize>> {
        if !self.ignore_case {
            return Ok(None);
        }
        let reached = std::ffi::CString::new(name).map_err(io::Error::other)?;
        let Some(stat) = fd.stat(&reached)? else {
            return Ok(None);
        };
        if (kind_of(stat.mode) == Kind::Dir) != dir {
            return Ok(None);
        }
        // a file may be that of two paths of the index at once, and so may
        // a directory be that of two directories of it
        for item in 0..names.len() {
            let same = |s: crate::snapshot::FileStat| (s.dev, s.ino) == (stat.dev, stat.ino);
            if (names.kind(item) == Kind::Dir) == dir
                && fd.stat(names.c_name(item))?.is_some_and(same)
            {
                return Ok(Some(item));
            }
        }
        Ok(None)
    }

    /// The entries `at` have no file by their `name`. A directory by that
    /// name is no untracked path, as for git; its files are, where each is
    /// asked for.
    fn no_file(
        &self,
        names: &Listing,
        name: &[u8],
        at: (usize, usize),
        reach: Reach,
        here: &mut Here,
        found: &mut Found,
    ) {
        if self.untracked == Untracked::Normal
            && let Some(dir) = names.find(name, true)
        {
            here.taken[dir] = true;
        }
        self.gone(at.0, at.1, reach, found);
    }

    /// Goes over what the listing `names` of the directory `base` has that
    /// the index does not: files no rule hides are untracked, directories
    /// none hides are walked.
    fn untracked_of(
        &self,
        base: &[u8],
        names: &Listing,
        ignore: &IgnoreStack,
        reach: Reach,
        here: &mut Here,
        found: &mut Found,
    ) {
        let mut path = base.to_vec();
        for item in (0..names.len()).filter(|&i| !here.taken[i]) {
            let name = names.name(item);
            if crate::control::outside(base.is_empty(), name) {
                continue;
            }
            let is_dir = names.kind(item) == Kind::Dir;
            // a FIFO, a socket or a device is no file a tree can hold
            let special = names.kind(item) == Kind::Other;
            with_name(&mut path, name, |path| match self.reach(reach, path) {
                Some(reach) if is_dir && !ignore.is_ignored(path, true) => {
                    found.unwalked.push(path.to_vec());
                    here.below.push(Below {
                        item,
                        tracked: None,
                        reach,
                    })
                }
                Some(Reach::Inside) if !is_dir && !special && !ignore.is_ignored(path, false) => {
                    found.untracked.push(path.to_vec())
                }
                Some(_) if is_dir => found.passed.push(path.to_vec()),
                _ => {}
            });
        }
    }

    /// Walks one of the directories found in the directory `base`, whose
    /// listing is `names`.
    fn visit(
        &self,
        fd: &DirFd,
        names: &Listing,
        base: &[u8],
        ignore: &IgnoreStack,
        below: &Below,
        twins: &[usize],
    ) -> io::Result<Found> {
        let mut found = Found::default();
        let sub = fd.open_at(names.c_name(below.item))?;
        match (below.tracked, sub) {
            (Some((dir, hidden)), Some(sub)) => {
                self.listed(sub, dir, twins, ignore, hidden, below.reach, &mut found)?
            }
            (Some((dir, _)), None) => std::iter::once(&dir)
                .chain(twins)
                .for_each(|&dir| self.all_gone(dir, below.reach, &mut found)),
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
    }
}
