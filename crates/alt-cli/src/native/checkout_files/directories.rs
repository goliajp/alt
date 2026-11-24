//! What a directory of the working tree holds, and the directories that
//! hold nothing: no tree has one, and a command that moves a directory
//! by name takes them along as changes of their own.

use super::super::checkout_recovery::{EMPTY_DIRECTORY, FileState};
use super::super::*;
use recovery::hash;

impl NativeRepo<'_> {
    /// The files in the directory at `path`, relative to the root; none
    /// when there is no directory of the working tree there: what a
    /// symlink above the path leads to is not looked at.
    pub(in crate::native) fn files_beneath(&self, path: &BString) -> Res<Vec<BString>> {
        let abs = self.abs(path)?;
        let mut found = Vec::new();
        let reached = self.steps_above(path)? == Ok(true);
        if !reached || !abs.symlink_metadata().is_ok_and(|m| m.is_dir()) {
            return Ok(found);
        }
        let mut stack = vec![abs];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    stack.push(entry.path());
                    continue;
                }
                let inner = entry.path();
                let rel = inner
                    .strip_prefix(&self.root)
                    .map_err(|_| "path outside root")?;
                found.push(self.as_tracked(rel.as_os_str().as_encoded_bytes())?);
            }
        }
        Ok(found)
    }

    /// The directories beneath `path` that hold nothing, relative to the
    /// root; none when no directory of the working tree is there.
    pub(in crate::native) fn empty_beneath(&self, path: &BString) -> Res<Vec<BString>> {
        let abs = self.abs(path)?;
        let mut found = Vec::new();
        let reached = self.steps_above(path)? == Ok(true);
        if !reached || !abs.symlink_metadata().is_ok_and(|m| m.is_dir()) {
            return Ok(found);
        }
        let mut stack = vec![abs];
        while let Some(dir) = stack.pop() {
            let mut holds = false;
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                holds = true;
                if entry.file_type()?.is_dir() {
                    stack.push(entry.path());
                }
            }
            if !holds {
                let rel = dir
                    .strip_prefix(&self.root)
                    .map_err(|_| "path outside root")?;
                found.push(self.as_tracked(rel.as_os_str().as_encoded_bytes())?);
            }
        }
        Ok(found)
    }

    /// The state of a directory that holds nothing.
    pub(in crate::native) fn empty_directory(&self, persist: bool) -> Res<FileState> {
        Ok(FileState {
            mode: EMPTY_DIRECTORY,
            contents: match persist {
                true => self.snapshot(&[])?,
                false => hash(&[]),
            },
        })
    }

    /// What a change of an empty directory finds at `path`: the directory
    /// where one is that holds nothing, and nothing where one holds
    /// something; what else is there as any change finds it.
    pub(in crate::native) fn observed_directory(
        &self,
        path: &BString,
        persist: bool,
        owned: &dyn Fn(&[u8]) -> bool,
    ) -> Res<Option<FileState>> {
        if self.validate_path_among(path, owned)? {
            return Ok(None);
        }
        let abs = self.abs(path)?;
        if !abs.symlink_metadata().is_ok_and(|m| m.is_dir()) {
            return self.file_state(path, persist);
        }
        match std::fs::read_dir(&abs)?.next() {
            Some(_) => Ok(None),
            None => Ok(Some(self.empty_directory(persist)?)),
        }
    }

    /// Makes the directory at `path`, where none is.
    pub(super) fn put_directory(&self, path: &BString, abs: &Path) -> Res<()> {
        self.make_directories_above(path)?;
        if !abs.symlink_metadata().is_ok_and(|m| m.is_dir()) {
            std::fs::create_dir(abs)?;
        }
        sync_parent(abs)
    }
}

/// Has the directory that holds `abs` reach the disk with its entries.
pub(super) fn sync_parent(abs: &Path) -> Res<()> {
    match abs.parent() {
        Some(parent) => Ok(std::fs::File::open(parent)?.sync_all()?),
        None => Ok(()),
    }
}
