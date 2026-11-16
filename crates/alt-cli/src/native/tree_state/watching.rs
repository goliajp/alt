//! The working trees a process that serves many commands watches for
//! changes, so that a scan looks only where something happened.

use std::path::Path;

use alt_worktree::{Delta, ScanOptions, Snapshot};

use super::Res;

/// By root; `None` for a tree that cannot be watched.
#[cfg(target_os = "linux")]
static WATCHED: std::sync::Mutex<
    Option<std::collections::HashMap<std::path::PathBuf, Option<alt_worktree::Watched>>>,
> = std::sync::Mutex::new(None);

/// The scan of the whole tree at `root`, from the events since the last
/// one where the tree is watched; `settings` are the files outside the
/// tree that decide how its files compare. `None` where it is not: in a
/// process that serves one command, for a tree that could not be watched
/// or can no longer be, and when the scan failed; the caller then scans
/// the tree itself.
#[cfg(target_os = "linux")]
pub(super) fn watched_scan(
    root: &Path,
    snapshot: &mut Snapshot,
    options: &ScanOptions<'_>,
    settings: &[&Path],
) -> Res<Option<Delta>> {
    if !super::HELD.started() {
        return Ok(None);
    }
    let mut guard = WATCHED.lock().unwrap();
    let trees = guard.get_or_insert_default();
    let tree = trees.entry(root.to_path_buf()).or_insert_with(|| {
        let most = alt_worktree::watch_allowance().ok()?;
        alt_worktree::Watched::start(root, most, snapshot).ok()
    });
    let Some(watched) = tree else {
        return Ok(None);
    };
    let delta = watched.scan(root, snapshot, options, settings);
    if delta.is_none() {
        *tree = None;
    }
    Ok(delta)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn watched_scan(
    _: &Path,
    _: &mut Snapshot,
    _: &ScanOptions<'_>,
    _: &[&Path],
) -> Res<Option<Delta>> {
    Ok(None)
}
