//! What a checkout will do, worked out before anything is written: the
//! checks that can refuse it, and the list of paths it changes.

use super::checkout_recovery::{CheckoutIntent, FileChange};
use super::*;
use recovery::canonical_index;

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

/// The files of the working tree that are not ignored, read when the first
/// directory standing in a file's way has to be judged.
#[derive(Default)]
struct Visible(Option<std::collections::HashSet<BString>>);

impl NativeRepo<'_> {
    /// The ignored files in the directory at `path`, which is about to
    /// become a file: they go with the directory, as in git. An untracked
    /// file that is not ignored refuses the checkout instead.
    fn ignored_in_the_way(
        &self,
        path: &BString,
        tracked: &dyn Fn(&[u8]) -> bool,
        visible: &mut Visible,
    ) -> Res<Vec<BString>> {
        let untracked: Vec<BString> = self
            .files_beneath(path)?
            .into_iter()
            .filter(|f| !tracked(f))
            .collect();
        if untracked.is_empty() {
            return Ok(untracked);
        }
        if visible.0.is_none() {
            let scan = self.scan_tree(&self.index()?)?;
            visible.0 = Some(scan.into_iter().map(|w| w.path).collect());
        }
        let seen = visible.0.as_ref().expect("just filled");
        match untracked.iter().any(|f| seen.contains(f)) {
            true => Err(holds_untracked(path)),
            false => Ok(untracked),
        }
    }

    pub(super) fn preflight_checkout(&self, old: &[WorkEntry], target: &[WorkEntry]) -> Res<()> {
        let old_paths: std::collections::HashSet<&[u8]> =
            old.iter().map(|e| e.path.as_slice()).collect();
        let tracked = |p: &[u8]| old_paths.contains(p);
        let same = unchanged(old, target);
        let mut visible = Visible::default();
        for entry in old.iter().filter(|e| !same.contains(e.path.as_slice())) {
            self.validate_path_among(&entry.path, &tracked)?;
        }
        for entry in target.iter().filter(|e| !same.contains(e.path.as_slice())) {
            let beneath_tracked = self.validate_path_among(&entry.path, &tracked)?;
            self.ensure_path_allowed(
                std::str::from_utf8(&entry.path)
                    .map_err(|_| "unsupported non-UTF-8 checkout path")?,
            )?;
            let on_disk = match beneath_tracked {
                true => None,
                false => self.abs(&entry.path)?.symlink_metadata().ok(),
            };
            match on_disk {
                Some(m) if m.is_dir() && entry.mode != 0o160000 => {
                    self.ignored_in_the_way(&entry.path, &tracked, &mut visible)?;
                }
                Some(m) if !m.is_dir() && !old_paths.contains(entry.path.as_slice()) => {
                    return Err(format!(
                        "untracked working tree file '{}' would be overwritten",
                        entry.path
                    )
                    .into());
                }
                _ => {}
            }
            if entry.mode != 0o160000 && self.store.odb.get(&entry.oid)?.is_none() {
                return Err("missing checkout object".into());
            }
        }
        for entry in old.iter().filter(|e| !same.contains(e.path.as_slice())) {
            self.ensure_path_allowed(
                std::str::from_utf8(&entry.path)
                    .map_err(|_| "unsupported non-UTF-8 checkout path")?,
            )?;
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
        let tracked = |p: &[u8]| old_paths.contains(p);
        let same = unchanged(old, target);
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
            let beneath_file = path
                .iter()
                .enumerate()
                .any(|(at, b)| *b == b'/' && files_on_disk.contains(&path.as_slice()[..at]));
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
        // ignored files in a directory that becomes a file are removed with
        // it, and recorded so that an undo brings them back
        for path in ignored {
            files.push(FileChange {
                before: self.file_state(&path, true)?,
                path: path.to_vec(),
                after: None,
            });
        }
        let index_after = target_index(target);
        let index_after = self.snapshot(&canonical_index(index_after, self.store.algo))?;
        *self.planned.borrow_mut() = Some(super::tree_state::Planned {
            index: index_after.clone(),
            files: planned,
        });
        Ok(CheckoutIntent {
            version: 1,
            workspace: self.workspace_identity()?,
            request_id: self.request_id.map(|id| crate::machine::hex(&id)),
            index_before: self.snapshot(&canonical_index(self.index()?, self.store.algo))?,
            index_after,
            files,
        })
    }
}

/// The index a checkout of `target` leaves in place.
fn target_index(target: &[WorkEntry]) -> Index {
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
    Index {
        version: 2,
        entries: target.iter().map(entry).collect(),
        extensions: Vec::new(),
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
