//! The directories the index knows nothing in: what a scan reports of them.

use std::io;
use std::ops::ControlFlow;

use crate::fsdir::{DirFd, Kind};
use crate::ignore::IgnoreStack;
use crate::walk::{Reach, Walk, holds_repository, ignore_file};

/// Which of the files the index does not have a scan reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Untracked {
    /// none: no directory is listed
    No,
    /// each file, but a directory with nothing tracked in it as one path
    /// ending in `/`, when it holds a file no ignore rule hides. Such a
    /// directory is looked into only until the first such file is found
    Normal,
    /// every file
    All,
}

impl Walk<'_> {
    /// Reports what an untracked directory holds: every file in it, or the
    /// directory itself. `path` is the directory's and is left as it was.
    pub(crate) fn untracked_in(
        &self,
        fd: DirFd,
        path: &mut Vec<u8>,
        ignore: &IgnoreStack,
        reach: Reach,
        out: &mut Vec<Vec<u8>>,
    ) -> io::Result<()> {
        if self.untracked == Untracked::All {
            let mut each = |file: &[u8]| {
                out.push(file.to_vec());
                ControlFlow::Continue(())
            };
            // never breaks
            let _ = self.each_file(fd, path, ignore, reach, &mut each)?;
            return Ok(());
        }
        let found = self.each_file(fd, path, ignore, reach, &mut |_| ControlFlow::Break(()))?;
        if found.is_break() {
            let mut dir = path.clone();
            dir.push(b'/');
            out.push(dir);
        }
        Ok(())
    }

    /// Calls `each` with every file beneath a directory the index knows
    /// nothing in, less what the ignore rules hide, until it breaks. A
    /// directory holding a repository is one path ending in `/`, as it is
    /// for git, and is not looked into. The files of a directory come
    /// before what lies in its subdirectories.
    fn each_file(
        &self,
        mut fd: DirFd,
        path: &mut Vec<u8>,
        ignore: &IgnoreStack,
        reach: Reach,
        each: &mut dyn FnMut(&[u8]) -> ControlFlow<()>,
    ) -> io::Result<ControlFlow<()>> {
        let mut names = fd.names()?;
        names.sort();
        if holds_repository(&names) {
            if reach != Reach::Inside {
                return Ok(ControlFlow::Continue(()));
            }
            path.push(b'/');
            let flow = each(path);
            path.pop();
            return Ok(flow);
        }
        let own = ignore_file(&fd, &names, path, ignore)?;
        let ignore = own.as_ref().unwrap_or(ignore);
        let mark = path.len();
        let mut dirs = Vec::new();
        for item in 0..names.len() {
            let name = names.name(item);
            if name == b".alt" {
                continue;
            }
            path.push(b'/');
            path.extend_from_slice(name);
            let is_dir = names.kind(item) == Kind::Dir;
            let flow = match self.reach(reach, path) {
                Some(reach) if is_dir && !ignore.is_ignored(path, true) => {
                    dirs.push((item, reach));
                    ControlFlow::Continue(())
                }
                Some(Reach::Inside) if !is_dir && !ignore.is_ignored(path, false) => each(path),
                _ => ControlFlow::Continue(()),
            };
            path.truncate(mark);
            if flow.is_break() {
                return Ok(flow);
            }
        }
        for (item, reach) in dirs {
            let Some(sub) = fd.open_at(names.c_name(item))? else {
                continue;
            };
            path.push(b'/');
            path.extend_from_slice(names.name(item));
            let flow = self.each_file(sub, path, ignore, reach, each)?;
            path.truncate(mark);
            if flow.is_break() {
                return Ok(flow);
            }
        }
        Ok(ControlFlow::Continue(()))
    }
}
