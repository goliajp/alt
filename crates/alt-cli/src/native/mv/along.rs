//! What a move finds at the new name, what goes along with a directory,
//! and where a move cannot go.

use std::os::unix::fs::MetadataExt;

use super::super::tree_edit::{Put, TreeEdit};
use super::super::*;

impl NativeRepo<'_> {
    /// Whether the file `there`, at the new name `to`, is the source
    /// `found` itself, and whether it is so as another spelling of its
    /// name: two links to a file are two names, each listed as it is
    /// spelled.
    pub(super) fn one_name(
        &self,
        found: &std::fs::Metadata,
        there: Option<&std::fs::Metadata>,
        to: &BString,
        folds: bool,
    ) -> Res<(bool, bool)> {
        let one =
            |there: &std::fs::Metadata| (there.dev(), there.ino()) == (found.dev(), found.ino());
        let one = there.is_some_and(one);
        let mut listed = super::checkout_files::Listed::default();
        Ok((
            one,
            one && folds && self.spelled_otherwise(to, &mut listed)?,
        ))
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
}
