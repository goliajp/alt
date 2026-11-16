//! The directories the index knows nothing in: what a scan reports of them.

use std::ffi::OsStr;
use std::io;
use std::ops::ControlFlow;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::fsdir::{DirFd, Kind, Listing};
use crate::ignore::{self, IgnoreStack};
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
    /// every file, those an ignore rule hides as well: no rule is read
    Hidden,
}

/// The outermost of `path` and the directories above it that an ignore
/// rule hides, by the ignore files `excludes` outside the tree at `root`
/// and those in it; `None` when no rule hides it.
pub fn hidden_by(root: &Path, path: &[u8], excludes: &[&Path]) -> io::Result<Option<Vec<u8>>> {
    let at = |rel: &[u8]| root.join(OsStr::from_bytes(rel));
    let mut rules = IgnoreStack::of_files(excludes)?;
    let mut from = 0usize;
    loop {
        let dir = &path[..from.saturating_sub(1)];
        let file = at(dir).join(".gitignore");
        // a symlink is not followed, as a scan does not follow one
        if std::fs::symlink_metadata(&file).is_ok_and(|meta| meta.is_file()) {
            rules.push(ignore::parse_layer(&std::fs::read(&file)?, dir));
        }
        let name = path[from..].iter().position(|b| *b == b'/');
        let end = name.map_or(path.len(), |len| from + len);
        let is_dir = |part: &[u8]| std::fs::symlink_metadata(at(part)).is_ok_and(|m| m.is_dir());
        if rules.is_ignored(&path[..end], end < path.len() || is_dir(path)) {
            return Ok(Some(path[..end].to_vec()));
        }
        if end == path.len() {
            return Ok(None);
        }
        from = end + 1;
    }
}

impl Walk<'_> {
    /// The rules that hold in a directory: `ignore` with those of its own
    /// ignore file on top, when it has one and the scan goes by rules.
    pub(crate) fn rules_in(
        &self,
        fd: &DirFd,
        names: &Listing,
        base: &[u8],
        ignore: &IgnoreStack,
    ) -> io::Result<Option<IgnoreStack>> {
        match self.untracked {
            Untracked::Hidden => Ok(None),
            _ => ignore_file(fd, names, base, ignore),
        }
    }

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
        if matches!(self.untracked, Untracked::All | Untracked::Hidden) {
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
            return Ok(as_one_path(path, each));
        }
        let own = self.rules_in(&fd, &names, path, ignore)?;
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

/// Calls `each` with the directory `path` itself, ending in `/`: a walk
/// limited to paths inside it finds the directory, not those paths.
fn as_one_path(
    path: &mut Vec<u8>,
    each: &mut dyn FnMut(&[u8]) -> ControlFlow<()>,
) -> ControlFlow<()> {
    path.push(b'/');
    let flow = each(path);
    path.pop();
    flow
}
