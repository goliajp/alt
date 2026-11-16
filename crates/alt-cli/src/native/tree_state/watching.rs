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
/// tree that decide how its files compare, and what else decides it. `None` where it is not: in a
/// process that serves one command, for a tree that could not be watched
/// or can no longer be, and when the scan failed; the caller then scans
/// the tree itself.
#[cfg(target_os = "linux")]
pub(super) fn watched_scan(
    root: &Path,
    snapshot: &mut Snapshot,
    options: &ScanOptions<'_>,
    settings: (&[&Path], &[u8]),
) -> Res<Option<Delta>> {
    if !super::HELD.started() {
        return Ok(None);
    }
    let mut guard = WATCHED.lock().unwrap();
    let trees = guard.get_or_insert_default();
    // a directory that is no working tree any more gives its watches back
    trees.retain(|at, _| at == root || at.join(".alt").exists());
    let tree = trees.entry(root.to_path_buf()).or_insert_with(|| {
        let most = alt_worktree::watch_allowance().ok()?;
        alt_worktree::Watched::start(root, most, snapshot).ok()
    });
    let Some(watched) = tree else {
        return Ok(None);
    };
    let delta = watched.scan(root, snapshot, options, settings.0, settings.1);
    if delta.is_none() {
        *tree = None;
    }
    Ok(delta)
}

/// Called for every file a command writes to the tree at `root`: now and
/// then the events of a watched tree are taken off their queue, which
/// holds fewer than a checkout of many files causes.
#[cfg(target_os = "linux")]
pub(in crate::native) fn wrote_file(root: &Path) {
    static WRITTEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let written = WRITTEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if !written.is_multiple_of(256) || !super::HELD.started() {
        return;
    }
    let mut guard = WATCHED.lock().unwrap();
    if let Some(Some(watched)) = guard.as_mut().and_then(|trees| trees.get_mut(root)) {
        watched.absorb();
    }
}

#[cfg(not(target_os = "linux"))]
pub(in crate::native) fn wrote_file(_: &Path) {}

#[cfg(not(target_os = "linux"))]
pub(super) fn watched_scan(
    _: &Path,
    _: &mut Snapshot,
    _: &ScanOptions<'_>,
    _: (&[&Path], &[u8]),
) -> Res<Option<Delta>> {
    Ok(None)
}
