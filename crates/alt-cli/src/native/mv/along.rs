//! What a move finds at the new name, what goes along with a directory,
//! and where a move cannot go.

use std::os::unix::fs::MetadataExt;

use super::super::tree_edit::{Put, TreeEdit};
use super::super::*;
use super::Moving;

impl NativeRepo<'_> {
    /// Whether the file `there`, at the new name of `moving`, is the
    /// source `found` itself, and whether it is so as another spelling
    /// of its name: two links to a file are two names, each listed as
    /// it is spelled, and of one name spelled in two ways the directory
    /// lists but one.
    pub(super) fn one_name(
        &self,
        found: &std::fs::Metadata,
        there: Option<&std::fs::Metadata>,
        moving: &Moving,
    ) -> Res<(bool, bool)> {
        let one =
            |there: &std::fs::Metadata| (there.dev(), there.ino()) == (found.dev(), found.ino());
        if !there.is_some_and(one) {
            return Ok((false, false));
        }
        let mut listed = super::super::checkout_files::Listed::default();
        let mut otherwise = |path: &BString| self.spelled_otherwise(path, &mut listed);
        let spelling = moving.folds && (otherwise(moving.to)? || otherwise(moving.from)?);
        Ok((true, spelling))
    }

    /// As git, which renames the directory: the directories in `from`
    /// that hold nothing go along to `to`.
    pub(super) fn move_hollow(&self, from: &BString, to: &BString, edit: &mut TreeEdit) -> Res<()> {
        for hollow in self.empty_beneath(from)? {
            let there = [to.as_slice(), &hollow[from.len()..]].concat();
            let state = self.empty_directory(true)?;
            edit.files.push((BString::from(there), Put::Found(state)));
            edit.files.push((hollow, Put::NoDirectory));
        }
        Ok(())
    }

    /// Refuses, in git's words, a move to another file system: git
    /// renames, and no rename goes from one file system to another.
    pub(super) fn ensure_one_device(&self, moving: &Moving) -> Res<()> {
        let device = |path: &Path| path.symlink_metadata().map(|found| found.dev());
        let target = self.abs(moving.to)?;
        let into = target.parent().ok_or("a path without a directory")?;
        if device(&self.abs(moving.from)?)? == device(into)? {
            return Ok(());
        }
        // what the system says of such a rename
        let crossed = std::io::Error::from_raw_os_error(18);
        let (from, why) = (moving.from, settings::reason(&crossed));
        Err(settings::fatal(format!("renaming '{from}' failed: {why}")))
    }
}
