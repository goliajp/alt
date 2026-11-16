//! The walk of a directory by its listing: what the index has in it is
//! matched against the names found, and the rest is untracked.

use std::io;

use super::{Below, Child, Found, Reach, Walk, with_name};
use crate::fsdir::{DirFd, Kind, Listing, kind_of};
use crate::ignore::IgnoreStack;

/// What a listed directory holds, as the walk of it sorts it out.
struct Here {
    /// which names of the listing the index has
    taken: Vec<bool>,
    /// the directories to walk
    below: Vec<Below>,
    /// the tracked files to look at: their entries and their name
    files: Vec<(usize, usize, usize)>,
}

impl Walk<'_> {
    /// Walks an index directory by its listing, which also shows what the
    /// index does not have. Inside an ignored directory (`hidden`) only the
    /// tracked files count.
    pub(super) fn listed(
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
        if !hidden {
            found.listed.push(dir as u32);
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
        };
        self.tracked_in(dir, &names, ignore, hidden, reach, &mut here, found);
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
        let visit = |below: &Below| self.visit(&fd, &names, base, ignore, below);
        self.below(dir, &here.below, visit, found)
    }

    /// Matches the children the index has in the directory `dir` against
    /// its listing `names`: files to look at and directories to walk go to
    /// `here`, what has nothing in place to `found`.
    #[expect(clippy::too_many_arguments, reason = "one step of `listed`")]
    fn tracked_in(
        &self,
        dir: usize,
        names: &Listing,
        ignore: &IgnoreStack,
        hidden: bool,
        reach: Reach,
        here: &mut Here,
        found: &mut Found,
    ) {
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
                    let Some(item) = names.find(name, false) else {
                        self.gone(first, end, reach, found);
                        continue;
                    };
                    here.taken[item] = true;
                    here.files.push((first, end, item));
                }
                Child::Dir(c) => match names.find(name, true) {
                    Some(item) => {
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
                    None => self.all_gone(c, reach, found),
                },
            }
        }
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
            if name == b".alt" || name == b".git" {
                continue;
            }
            let is_dir = names.kind(item) == Kind::Dir;
            with_name(&mut path, name, |path| match self.reach(reach, path) {
                Some(reach) if is_dir && !ignore.is_ignored(path, true) => {
                    found.unwalked.push(path.to_vec());
                    here.below.push(Below {
                        item,
                        tracked: None,
                        reach,
                    })
                }
                Some(Reach::Inside) if !is_dir && !ignore.is_ignored(path, false) => {
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
    ) -> io::Result<Found> {
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
    }
}
