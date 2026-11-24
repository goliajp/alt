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
    /// workspace may lie on another than the store. Asked once of a
    /// command.
    pub(in crate::native) fn folds(&self) -> Res<bool> {
        if let Some(known) = self.folded.get() {
            return Ok(*known);
        }
        let set = crate::git_settings::ignore_case(&self.settings()?)?;
        let folds = set || import_config::folds_case(&self.root.join(".alt"));
        Ok(*self.folded.get_or_init(|| folds))
    }

    /// Names each of `changes` that writes a file again at a path the
    /// index `restored` lacks by the path of `restored`, one `current`
    /// lacks, whose file it reaches under another spelling: written
    /// there, the file takes the spelling the index it comes back to has
    /// for it.
    pub(in crate::native) fn named_as_indexed(
        &self,
        mut changes: Vec<FileChange>,
        restored: &Index,
        current: &Index,
    ) -> Res<Vec<FileChange>> {
        if !self.folds()? {
            return Ok(changes);
        }
        let paths = |index: &Index| -> HashSet<Vec<u8>> {
            index.entries.iter().map(|e| e.path.to_vec()).collect()
        };
        let (back, now) = (paths(restored), paths(current));
        let changed: HashSet<Vec<u8>> = changes.iter().map(|c| c.path.clone()).collect();
        let names = restored.entries.iter().map(|e| &e.path);
        let names: Vec<&BString> = names
            .filter(|p| !now.contains(p.as_slice()) && !changed.contains(p.as_slice()))
            .collect();
        for change in &mut changes {
            let file = |state: &Option<FileState>| {
                state.as_ref().is_some_and(|state| state.mode != 0o160000)
            };
            if !file(&change.before) || !file(&change.after) || back.contains(&change.path) {
                continue;
            }
            let path = BString::from(change.path.clone());
            if let Some(named) = self.shares(&path, names.iter().copied())? {
                change.path = named.to_vec();
            }
        }
        self.named_as_folded(&mut changes, &back, &names)?;
        Ok(changes)
    }

    /// Which file is at `path`, to the file system; `None` where none is.
    pub(in crate::native) fn file_there(&self, path: &BString) -> Res<Option<(u64, u64)>> {
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
        // as git: a name is another spelling of one the index has only
        // where the two differ in the case of letters of the alphabet
        let there = self.file_there(path)?;
        for other in gone.filter(|other| *other != path && other.eq_ignore_ascii_case(path)) {
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

    /// The path among `tracked` whose file `path` reaches by another
    /// spelling of its name; `None` where the file at `path` is no
    /// tracked path's.
    pub(in crate::native) fn shares<'p>(
        &self,
        path: &BString,
        tracked: impl Iterator<Item = &'p BString>,
    ) -> Res<Option<BString>> {
        if !self.folds()? {
            return Ok(None);
        }
        let there = self.file_there(path)?;
        let mut listed = Listed::default();
        for other in tracked.filter(|other| *other != path) {
            // two links to a file are two names, each listed as it is spelled
            if there.is_some()
                && self.file_there(other)? == there
                && (self.spelled_otherwise(path, &mut listed)?
                    || self.spelled_otherwise(other, &mut listed)?)
            {
                return Ok(Some(other.clone()));
            }
        }
        Ok(None)
    }

    /// Why the file at `path`, which the index does not have, stops a
    /// checkout that brings the path: it is another tracked path's file
    /// under another spelling, or it is not tracked.
    pub(in crate::native) fn why_in_the_way<'p>(
        &self,
        path: &BString,
        tracked: impl Iterator<Item = &'p BString>,
    ) -> Res<String> {
        Ok(match self.shares(path, tracked)? {
            Some(other) => one_file(path, &other),
            None => format!("untracked working tree file '{path}' would be overwritten"),
        })
    }

    /// Refuses the checkout that the files at `in_the_way` stop where one
    /// of them is the file of a path among `tracked`: it is not an
    /// untracked file to move away.
    pub(in crate::native) fn ensure_apart<'p>(
        &self,
        in_the_way: &[BString],
        tracked: impl Iterator<Item = &'p BString> + Clone,
    ) -> Res<()> {
        for path in in_the_way {
            if let Some(other) = self.shares(path, tracked.clone())? {
                let said = format!("error: {}\n", one_file(path, &other));
                return Err(said::refused(&said, 1));
            }
        }
        Ok(())
    }

    /// The paths among `entries` that reach one file with another of
    /// them, each group in the order of its paths: of a group the tree
    /// holds one file, with what the path written last put there.
    pub(in crate::native) fn collided(&self, entries: &[WorkEntry]) -> Res<Vec<Vec<BString>>> {
        if !self.folds()? {
            return Ok(Vec::new());
        }
        let mut reached: std::collections::BTreeMap<(u64, u64), Vec<BString>> = Default::default();
        for entry in entries.iter().filter(|e| e.mode != 0o160000) {
            if let Some(file) = self.file_there(&entry.path)? {
                reached.entry(file).or_default().push(entry.path.clone());
            }
        }
        let mut groups: Vec<_> = reached.into_values().filter(|g| g.len() > 1).collect();
        groups.sort();
        Ok(groups)
    }
}

/// Why a path is not checked out beside the tracked path `other` whose
/// file it reaches.
pub(in crate::native) fn one_file(path: &BString, other: &BString) -> String {
    format!(
        "'{path}' and '{other}' are one file here, where names that differ in case are one name: a tree that holds both is not checked out"
    )
}
