//! What a checkout will do, worked out before anything is written: the
//! checks that can refuse it, and the list of paths it changes.

use super::checkout_files::apart::{Coming, respell};
use super::checkout_recovery::{CheckoutIntent, FileChange};
use super::*;
use recovery::canonical_index;

mod in_the_way;

pub(super) use in_the_way::Visible;

/// The paths whose content and mode are the same in both trees. A checkout
/// leaves them alone, whatever the working tree holds there.
fn unchanged<'a>(
    old: &'a [WorkEntry],
    target: &'a [WorkEntry],
) -> std::collections::HashSet<&'a [u8]> {
    let before: std::collections::HashMap<&[u8], &WorkEntry> =
        old.iter().map(|e| (e.path.as_slice(), e)).collect();
    target
        .iter()
        .filter(|t| {
            before
                .get(t.path.as_slice())
                .is_some_and(|o| o.oid == t.oid && o.mode == t.mode)
        })
        .map(|t| t.path.as_slice())
        .collect()
}

impl NativeRepo<'_> {
    /// Ends the list of what a checkout changes: a file that a path
    /// which comes reaches by another spelling is not removed, and the
    /// ignored files in a directory that becomes a file are removed with
    /// it, recorded so that an undo brings them back.
    fn settle_changes<'p>(
        &self,
        files: &mut Vec<FileChange>,
        come: impl Iterator<Item = &'p BString>,
        ignored: Vec<BString>,
    ) -> Res<()> {
        self.keep_respelled(files, come)?;
        for path in ignored {
            files.push(FileChange {
                before: self.file_state(&path, true)?,
                path: path.to_vec(),
                after: None,
            });
        }
        Ok(())
    }

    /// What refuses a checkout before anything is written.
    pub(super) fn preflight_checkout(&self, old: &[WorkEntry], target: &[WorkEntry]) -> Res<()> {
        self.preflight_paths(old, target)?;
        let coming = Coming::of(old, target, &unchanged(old, target));
        self.ensure_new_apart(&coming).map_err(plainly)
    }

    /// The checks of [`preflight_checkout`](Self::preflight_checkout) but
    /// the one that the paths which come are apart from each other, which
    /// an import has settled by leaving paths out.
    pub(super) fn preflight_paths(&self, old: &[WorkEntry], target: &[WorkEntry]) -> Res<()> {
        let old_paths: std::collections::HashSet<&[u8]> =
            old.iter().map(|e| e.path.as_slice()).collect();
        let same = unchanged(old, target);
        let coming = Coming::of(old, target, &same);
        let respelled = self.respelled_by(&coming).map_err(plainly)?;
        let tracked = |p: &[u8]| old_paths.contains(p) || respelled.contains_key(p);
        let mut visible = Visible::default();
        for entry in old.iter().filter(|e| !same.contains(e.path.as_slice())) {
            self.validate_path_among(&entry.path, &tracked)?;
        }
        for entry in target.iter().filter(|e| !same.contains(e.path.as_slice())) {
            let beneath_tracked = self.validate_path_among(&entry.path, &tracked)?;
            self.ensure_path_allowed(&entry.path)?;
            let on_disk = match beneath_tracked {
                true => None,
                false => self.abs(&entry.path)?.symlink_metadata().ok(),
            };
            match on_disk {
                Some(m) if m.is_dir() && entry.mode != 0o160000 => {
                    self.ignored_in_the_way(&entry.path, &tracked, &mut visible)?;
                }
                Some(m) if !m.is_dir() && !old_paths.contains(entry.path.as_slice()) => {
                    let stays: std::collections::HashSet<&BString> =
                        target.iter().map(|e| &e.path).collect();
                    let leaving = old.iter().map(|e| &e.path).filter(|p| !stays.contains(p));
                    if !self.respells(&entry.path, leaving)? {
                        let tracked = old.iter().map(|e| &e.path);
                        return Err(self.why_in_the_way(&entry.path, tracked)?.into());
                    }
                }
                _ => {}
            }
            if entry.mode != 0o160000 && self.store.odb.get(&entry.oid)?.is_none() {
                return Err("missing checkout object".into());
            }
        }
        for entry in old.iter().filter(|e| !same.contains(e.path.as_slice())) {
            self.ensure_path_allowed(&entry.path)?;
        }
        Ok(())
    }

    pub(super) fn prepare_checkout(
        &self,
        old: &[WorkEntry],
        target: &[WorkEntry],
    ) -> Res<CheckoutIntent> {
        let targets: std::collections::BTreeMap<_, _> =
            target.iter().map(|t| (&t.path, t)).collect();
        let old_paths: std::collections::HashSet<&[u8]> =
            old.iter().map(|e| e.path.as_slice()).collect();
        let same = unchanged(old, target);
        let respelled = self.respelled_by(&Coming::of(old, target, &same));
        let respelled = respelled.map_err(plainly)?;
        let tracked = |p: &[u8]| old_paths.contains(p) || respelled.contains_key(p);
        let paths: std::collections::BTreeSet<_> = old
            .iter()
            .chain(target)
            .map(|e| &e.path)
            .filter(|p| !same.contains(p.as_slice()))
            .collect();
        let gitlinks: std::collections::HashSet<&BString> = old
            .iter()
            .chain(target)
            .filter(|e| e.mode == 0o160000)
            .map(|e| &e.path)
            .collect();
        let attrs = self.converter_for(target)?;
        // paths found as a file or symlink: the preflight has made sure
        // nothing but a tracked path stands above another as a non-directory
        let mut files_on_disk: std::collections::HashSet<&[u8]> = std::collections::HashSet::new();
        let mut visible = Visible::default();
        let mut ignored = Vec::new();
        let mut files = Vec::new();
        let mut planned = std::collections::HashMap::new();
        for path in paths {
            let file =
                |above: &[u8]| files_on_disk.contains(above) || respelled.contains_key(above);
            let mut parents = path.iter().enumerate().filter(|(_, b)| **b == b'/');
            let beneath_file = parents.any(|(at, _)| file(&path.as_slice()[..at]));
            let found = match beneath_file {
                true => None,
                false => self.file_state(path, true)?,
            };
            let directory = found.as_ref().is_some_and(|s| s.mode == 0o160000);
            let before = found.filter(|_| !directory || gitlinks.contains(path));
            if before.is_some() && !gitlinks.contains(path) {
                files_on_disk.insert(path.as_slice());
            }
            let after = targets
                .get(path)
                .map(|t| self.target_state(t, &attrs))
                .transpose()?;
            if let Some(state) = &after {
                planned.insert(path.to_vec(), (state.mode, state.contents.clone()));
            }
            // the directory of a submodule goes with its entry while it is
            // empty; what it holds is not this checkout's to remove
            if before.as_ref().is_some_and(|s| s.mode == 0o160000)
                && after.is_none()
                && std::fs::read_dir(self.abs(path)?)?.next().is_some()
            {
                left_in_place(path)?;
                continue;
            }
            if directory && after.as_ref().is_some_and(|s| s.mode != 0o160000) {
                ignored.extend(self.ignored_in_the_way(path, &tracked, &mut visible)?);
            }
            if before != after {
                files.push(FileChange {
                    path: path.to_vec(),
                    before,
                    after,
                });
            }
        }
        respell(&mut files, &respelled);
        let come = target.iter().map(|e| &e.path);
        let come = come.filter(|p| !old_paths.contains(p.as_slice()));
        self.settle_changes(&mut files, come, ignored)?;
        let index_after = target_index(target)?;
        let index_after = self.snapshot(&canonical_index(index_after, self.store.algo()?))?;
        *self.planned.borrow_mut() = Some(super::tree_state::Planned {
            index: index_after.clone(),
            files: planned,
        });
        Ok(CheckoutIntent {
            version: 1,
            workspace: self.workspace_identity()?,
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            index_before: self.snapshot(&canonical_index(self.index()?, self.store.algo()?))?,
            index_after,
            files,
        })
    }
}

/// The index a checkout of `target` leaves in place, refused where a path
/// of it would be a file and a directory.
fn target_index(target: &[WorkEntry]) -> Res<Index> {
    let entry = |entry: &WorkEntry| IndexEntry {
        ctime: (0, 0),
        mtime: (0, 0),
        dev: 0,
        ino: 0,
        mode: entry.mode,
        uid: 0,
        gid: 0,
        size: 0,
        oid: entry.oid,
        flags: entry.path.len().min(0x0fff) as u16,
        extended_flags: None,
        path: entry.path.clone(),
    };
    let index = Index {
        version: 2,
        entries: target.iter().map(entry).collect(),
        extensions: Vec::new(),
    };
    index_file::ensure_sound(&index)?;
    Ok(index)
}

/// A refusal as a checkout says what stands in its way: as its own
/// failure, like a file that is not tracked.
fn plainly(error: Box<dyn std::error::Error>) -> Box<dyn std::error::Error> {
    match error.downcast::<crate::machine::Failure>() {
        Ok(refusal) => refusal.message.into(),
        Err(other) => other,
    }
}

/// Says of a submodule directory left in place what git says. A command
/// that answers in JSON has no place for it.
pub(super) fn left_in_place(path: &BString) -> Res<()> {
    if !crate::diagnostics::json_mode() {
        let said = format!("warning: unable to rmdir '{path}': Directory not empty\n");
        crate::diagnostics::emit(said.as_bytes())?;
    }
    Ok(())
}

/// Removes the directory of a submodule at `abs`, which was empty when
/// the checkout was planned; one that holds something by now stays.
pub(super) fn remove_left_empty(abs: &Path, path: &BString) -> Res<()> {
    match std::fs::remove_dir(abs) {
        Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => left_in_place(path),
        removed => Ok(removed?),
    }
}

pub(super) fn holds_untracked(path: &BString) -> Box<dyn std::error::Error> {
    crate::machine::Failure::refusal(
        "precondition_failed",
        format!("checkout target '{path}' is a directory holding untracked files"),
    )
    .into()
}
