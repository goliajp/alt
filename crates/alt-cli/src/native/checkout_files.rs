//! What a checkout reads from and writes to the working tree, one path at a
//! time, and the checks that run before the first write.

use super::checkout_plan::holds_untracked;
use super::checkout_recovery::{CheckoutIntent, FileChange, FileState};
use super::*;
use recovery::hash;

pub(super) mod apart;
mod asked;
mod folded;
mod moves;
mod paths;

pub(super) use asked::clear_left;
pub(super) use paths::ensure_no_control_directory;

impl NativeRepo<'_> {
    pub(super) fn file_state(&self, path: &BString, persist: bool) -> Res<Option<FileState>> {
        use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
        let abs = self.abs(path)?;
        match abs.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() => {
                let bytes = std::fs::read_link(abs)?;
                Ok(Some(FileState {
                    mode: 0o120000,
                    contents: if persist {
                        self.snapshot(bytes.as_os_str().as_bytes())?
                    } else {
                        hash(bytes.as_os_str().as_bytes())
                    },
                }))
            }
            Ok(meta) if meta.is_file() => {
                let mode = if meta.permissions().mode() & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                };
                Ok(Some(FileState {
                    mode,
                    contents: {
                        let bytes = std::fs::read(abs)?;
                        if persist {
                            self.snapshot(&bytes)?
                        } else {
                            hash(&bytes)
                        }
                    },
                }))
            }
            Ok(meta) if meta.is_dir() => Ok(Some(FileState {
                mode: 0o160000,
                contents: String::new(),
            })),
            Ok(_) => Err("checkout encountered unsupported special file".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// What a checkout that manages the `owned` paths finds at `path`.
    /// Nothing lives beneath an owned file or symlink. A directory is a
    /// submodule placeholder where `gitlink` expects one; anywhere else it
    /// only holds other paths and is not content itself.
    pub(super) fn observed(
        &self,
        path: &BString,
        persist: bool,
        gitlink: bool,
        owned: &dyn Fn(&[u8]) -> bool,
    ) -> Res<Option<FileState>> {
        if self.validate_path_among(path, owned)? {
            return Ok(None);
        }
        Ok(self
            .file_state(path, persist)?
            .filter(|state| state.mode != 0o160000 || gitlink))
    }

    /// [`observed`](Self::observed) for one recorded change among `paths`,
    /// the paths of every change of the same checkout.
    pub(super) fn observed_among(
        &self,
        paths: &std::collections::HashSet<&[u8]>,
        change: &FileChange,
    ) -> Res<Option<FileState>> {
        let gitlink = [&change.before, &change.after]
            .into_iter()
            .flatten()
            .any(|state| state.mode == 0o160000);
        self.observed(&BString::from(change.path.clone()), false, gitlink, &|p| {
            paths.contains(p)
        })
    }

    /// The working-tree state `entry` is checked out as, under the
    /// attributes of the tree it belongs to.
    pub(super) fn target_state(
        &self,
        entry: &WorkEntry,
        attrs: &convert::Converter,
    ) -> Res<FileState> {
        if entry.mode == 0o160000 {
            return Ok(FileState {
                mode: entry.mode,
                contents: String::new(),
            });
        }
        let object = self
            .store
            .odb
            .get(&entry.oid)?
            .ok_or("missing checkout object")?;
        let bytes = if entry.mode != 0o120000 {
            match alt_lfs::Pointer::parse(&object.data) {
                Some(pointer) => self.lfs_content(&pointer)?.unwrap_or(object.data),
                None => attrs
                    .to_worktree(&entry.path, &object.data)?
                    .unwrap_or(object.data),
            }
        } else {
            object.data
        };
        Ok(FileState {
            mode: entry.mode,
            contents: self.snapshot(&bytes)?,
        })
    }

    /// Applies recorded changes. What leaves goes first: a path can only
    /// turn from a file or a symlink into a directory, or back, once the
    /// old side is out of the way. A file that only moves is renamed
    /// then, and what could not be renamed leaves and is written with
    /// the rest.
    pub(super) fn apply_changes<'c>(
        &self,
        changes: impl Iterator<Item = &'c FileChange> + Clone,
    ) -> Res<()> {
        let removed = |c: &&FileChange| c.after.is_none();
        let apply = |change: &FileChange| -> Res<()> {
            crash_point!("checkout-file:before");
            let path = BString::from(change.path.clone());
            let applied = self.apply_file_state(&path, &change.after);
            applied.map_err(|e| format!("cannot write '{path}': {e}").into())
        };
        let all: Vec<&FileChange> = changes.collect();
        let moves = moves::moves_among(&all);
        let stays = |c: &&FileChange| !moves.contains_key(c.path.as_slice());
        let leaving = all.iter().copied().filter(removed);
        leaving.filter(stays).try_for_each(apply)?;
        let moved = self.rename_moved(&all, &moves)?;
        let rest = || {
            all.iter()
                .copied()
                .filter(|c| !moved.contains(c.path.as_slice()))
        };
        rest()
            .filter(removed)
            .filter(|c| !stays(c))
            .try_for_each(apply)?;
        rest().filter(|c| !removed(c)).try_for_each(apply)
    }

    /// Refuses, before anything is written, a change that would put a file
    /// where a directory still holds something the changes do not remove.
    pub(super) fn ensure_directories_replaceable(&self, intent: &CheckoutIntent) -> Res<()> {
        let removed: std::collections::HashSet<&[u8]> = intent
            .files
            .iter()
            .filter(|c| c.after.is_none())
            .map(|c| c.path.as_slice())
            .collect();
        for change in &intent.files {
            if change.after.as_ref().is_some_and(|s| s.mode != 0o160000) {
                self.ensure_replaceable(&BString::from(change.path.clone()), &|p| {
                    removed.contains(p)
                })?;
            }
        }
        Ok(())
    }

    /// A directory at `path` may give way to a file only when every file in
    /// it is one the checkout `removes`.
    fn ensure_replaceable(&self, path: &BString, removes: &dyn Fn(&[u8]) -> bool) -> Res<()> {
        match self.files_beneath(path)?.iter().all(|f| removes(f)) {
            true => Ok(()),
            false => Err(holds_untracked(path)),
        }
    }

    /// The files in the directory at `path`, relative to the root; none
    /// when there is no directory of the working tree there: what a
    /// symlink above the path leads to is not looked at.
    pub(super) fn files_beneath(&self, path: &BString) -> Res<Vec<BString>> {
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
                found.push(BString::from(rel.as_os_str().as_encoded_bytes()));
            }
        }
        Ok(found)
    }

    fn apply_file_state(&self, path: &BString, state: &Option<FileState>) -> Res<()> {
        super::tree_state::wrote_file(&self.root);
        let abs = self.abs(path)?;
        match state {
            None => {
                self.beneath_directories(path)?;
                match abs.symlink_metadata().is_ok_and(|m| m.is_dir()) {
                    true => super::checkout_plan::remove_left_empty(&abs, path)?,
                    false => crate::durable::remove_file(&abs)?,
                }
                if let Some(parent) = abs.parent() {
                    std::fs::File::open(parent)?.sync_all()?;
                }
                self.prune_empty_dirs(abs.parent());
            }
            Some(state) if state.mode == 0o160000 => {
                self.make_directories_above(path)?;
                match abs.symlink_metadata() {
                    Ok(found) if found.is_dir() => {}
                    Ok(_) => {
                        crate::durable::remove_file(&abs)?;
                        std::fs::create_dir(&abs)?;
                    }
                    Err(_) => std::fs::create_dir(&abs)?,
                }
                std::fs::File::open(&abs)?.sync_all()?;
                if let Some(parent) = abs.parent() {
                    std::fs::File::open(parent)?.sync_all()?;
                }
            }
            Some(state) => {
                let bytes = self.read_snapshot(&state.contents)?;
                let parent = abs.parent().ok_or("checkout path has no parent")?;
                self.make_directories_above(path)?;
                paths::remove_empty_directories(&abs)?;
                self.remove_spelled_otherwise(path, &abs)?;
                if state.mode == 0o120000 {
                    let temp = alt_os::temp::tempdir_in(parent)?;
                    let link = temp.path().join("link");
                    symlink_to(&bytes, &link)?;
                    crate::durable::rename(link, &abs)?;
                    self.note_written(path, &abs, state)?;
                    std::fs::File::open(parent)?.sync_all()?;
                } else {
                    use std::os::unix::fs::PermissionsExt;
                    let mut tmp = alt_os::temp::NamedTempFile::new_in(parent)?;
                    tmp.write_all(&bytes)?;
                    tmp.as_file()
                        .set_permissions(std::fs::Permissions::from_mode(
                            if state.mode == 0o100755 { 0o755 } else { 0o644 },
                        ))?;
                    tmp.as_file().sync_all()?;
                    tmp.persist(&abs)?;
                    self.note_written(path, &abs, state)?;
                    std::fs::File::open(parent)?.sync_all()?;
                }
            }
        }
        self.sync_parents(&abs)
    }

    /// Remembers what a file looks like right after the checkout wrote it,
    /// for the snapshot of the index the checkout ends with. Only a file
    /// written as the checkout planned from a tree's entries counts: an
    /// undo or a recovery writes back what it once found there, which
    /// need not be what the index holds.
    fn note_written(&self, path: &BString, abs: &Path, state: &FileState) -> Res<()> {
        let planned = self.planned.borrow();
        let as_planned = planned.as_ref().and_then(|p| p.files.get(path.as_slice()));
        if as_planned == Some(&(state.mode, state.contents.clone())) {
            let stat = alt_worktree::FileStat::of(&std::fs::symlink_metadata(abs)?);
            self.written.borrow_mut().push((path.clone(), stat));
        }
        Ok(())
    }
}
