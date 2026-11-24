//! A file system that takes names differing in case for one: there a
//! path a checkout brings may reach the file of a tracked path it takes
//! away. That file is not in the way of the checkout, and it is not
//! removed: it is the file of the path that comes.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;

use super::*;

impl NativeRepo<'_> {
    /// Whether names that differ in case are one name here: where
    /// `core.ignoreCase` says so, and where the file system of this
    /// working tree shows it. The configuration of an imported store was
    /// written on the file system of the git repository, and a parallel
    /// workspace may lie on another than the store.
    pub(in crate::native) fn folds(&self) -> Res<bool> {
        let set = crate::git_settings::ignore_case(&self.settings()?)?;
        Ok(set || import_config::folds_case(&self.root.join(".alt")))
    }

    /// Which file is at `path`, to the file system; `None` where none is.
    fn file_there(&self, path: &BString) -> Res<Option<(u64, u64)>> {
        let found = self.abs(path)?.symlink_metadata().ok();
        Ok(found.map(|m| (m.dev(), m.ino())))
    }

    /// Whether the file found at `path`, a path the index does not have,
    /// is the file of one of the tracked paths `gone` that the checkout
    /// takes away, reached by another spelling of its name.
    pub(in crate::native) fn respells<'p>(
        &self,
        path: &BString,
        gone: impl Iterator<Item = &'p BString>,
    ) -> Res<bool> {
        if !self.folds()? {
            return Ok(false);
        }
        let there = self.file_there(path)?;
        for other in gone.filter(|other| *other != path) {
            if there.is_some() && self.file_there(other)? == there {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Takes out of `changes` the removal of each file that one of the
    /// paths `come`, which the checkout leaves a file at, reaches by
    /// another spelling: removing it would remove the file that path has.
    pub(in crate::native) fn keep_respelled<'p>(
        &self,
        changes: &mut Vec<FileChange>,
        come: impl Iterator<Item = &'p BString>,
    ) -> Res<()> {
        let removed = |c: &FileChange| c.after.is_none() && c.before.is_some();
        if !changes.iter().any(removed) || !self.folds()? {
            return Ok(());
        }
        let leaving: HashSet<&[u8]> = changes
            .iter()
            .filter(|c| removed(c))
            .map(|c| c.path.as_slice())
            .collect();
        let mut reached = HashSet::new();
        for path in come.filter(|path| !leaving.contains(path.as_slice())) {
            reached.extend(self.file_there(path)?);
        }
        let mut kept = Vec::with_capacity(changes.len());
        for change in changes.drain(..) {
            let there = match removed(&change) {
                true => self.file_there(&BString::from(change.path.clone()))?,
                false => None,
            };
            if !there.is_some_and(|file| reached.contains(&file)) {
                kept.push(change);
            }
        }
        *changes = kept;
        Ok(())
    }
}
