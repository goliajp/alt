//! The working trees a process that serves many commands watches for
//! changes, so that a scan looks only where something happened.

#[cfg(not(target_os = "linux"))]
use std::path::Path;

#[cfg(not(target_os = "linux"))]
use alt_worktree::{Delta, ScanOptions, Snapshot};

#[cfg(not(target_os = "linux"))]
use super::Res;

#[cfg(target_os = "linux")]
pub(super) use linux::watched_scan;
#[cfg(target_os = "linux")]
pub(in crate::native) use linux::wrote_file;

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

    use alt_worktree::{Delta, ScanOptions, Snapshot, Watched};

    use super::super::Res;

    /// What decides whether a tree can be watched, as far as it can change
    /// while the process runs.
    #[derive(PartialEq, Eq)]
    pub(super) struct Basis {
        /// the device and inode of the root
        root: Option<(u64, u64)>,
        /// what is mounted where
        mounts: Vec<u8>,
    }

    impl Basis {
        fn of(root: &Path) -> Basis {
            let root = std::fs::metadata(root).ok().map(|m| (m.dev(), m.ino()));
            let mounts = std::fs::read("/proc/self/mountinfo").unwrap_or_default();
            Basis { root, mounts }
        }
    }

    pub(super) enum State {
        /// watching has not been tried
        New,
        Watched(Box<Watched>),
        /// not watched: tried again once the tree is on another basis than
        /// it was when watching failed or ended, never when there is none
        Off(Option<Basis>),
    }

    type Tree = Arc<Mutex<State>>;

    /// By root. The lock on the table is held to look a tree up, the lock
    /// of a tree for as long as a scan of it takes.
    static TREES: Mutex<Option<HashMap<PathBuf, Tree>>> = Mutex::new(None);

    /// The tree at `root`, put into the table when it is not there.
    fn tree(root: &Path) -> Tree {
        let gone: Vec<Tree>;
        let tree = {
            // nothing done under this lock leaves the table half changed
            let mut guard = TREES.lock().unwrap_or_else(PoisonError::into_inner);
            let trees = guard.get_or_insert_default();
            // a directory that is no working tree any more gives its watches back
            let stays = |at: &PathBuf| at == root || at.join(".alt").exists();
            let left: Vec<PathBuf> = trees.keys().filter(|at| !stays(at)).cloned().collect();
            gone = left.iter().filter_map(|at| trees.remove(at)).collect();
            let new = || Arc::new(Mutex::new(State::New));
            trees.entry(root.to_path_buf()).or_insert_with(new).clone()
        };
        // the watches are given back with the table free for others
        drop(gone);
        tree
    }

    /// The state of a tree, for this thread alone. A thread that panicked
    /// with the state in hand may have left it half changed: the tree is
    /// not watched from then on, and commands scan it themselves.
    pub(super) fn locked(tree: &Mutex<State>) -> MutexGuard<'_, State> {
        tree.lock().unwrap_or_else(|poisoned| {
            let mut state = poisoned.into_inner();
            *state = State::Off(None);
            tree.clear_poison();
            state
        })
    }

    /// Whether to try and watch a tree in this state, which is on the
    /// basis `now`.
    pub(super) fn due(state: &State, now: impl FnOnce() -> Basis) -> bool {
        match state {
            State::New => true,
            State::Off(Some(then)) => *then != now(),
            State::Off(None) | State::Watched(_) => false,
        }
    }

    /// What a tree is after watching it failed with `error`: running out
    /// of watches, or of the queues they are on, does not pass by itself.
    fn off(error: &std::io::Error, basis: Basis) -> State {
        const ENOSPC: i32 = 28;
        const EMFILE: i32 = 24;
        match error.raw_os_error() {
            Some(ENOSPC | EMFILE) => State::Off(None),
            _ => State::Off(Some(basis)),
        }
    }

    fn start(root: &Path, snapshot: &Snapshot, options: &ScanOptions<'_>) -> State {
        // before the attempt: what changes during it is another basis next time
        let basis = Basis::of(root);
        let started = alt_worktree::watch_allowance()
            .and_then(|most| Watched::start(root, most, snapshot, options));
        match started {
            Ok(watched) => State::Watched(Box::new(watched)),
            Err(e) => off(&e, basis),
        }
    }

    /// The scan of the whole tree at `root`, from the events since the
    /// last one where the tree is watched; `settings` are the files
    /// outside the tree that decide how its files compare, and what else
    /// decides it. `None` where it is not: in a process that serves one
    /// command, for a tree that could not be watched or can no longer be,
    /// and when the scan failed; the caller then scans the tree itself.
    pub(in crate::native::tree_state) fn watched_scan(
        root: &Path,
        snapshot: &mut Snapshot,
        options: &ScanOptions<'_>,
        settings: (&[&Path], &[u8]),
    ) -> Res<Option<Delta>> {
        if !super::super::HELD.started() {
            return Ok(None);
        }
        let tree = tree(root);
        let mut state = locked(&tree);
        if due(&state, || Basis::of(root)) {
            *state = start(root, snapshot, options);
        }
        let State::Watched(watched) = &mut *state else {
            return Ok(None);
        };
        let delta = watched.scan(root, snapshot, options, settings.0, settings.1);
        if delta.is_none() {
            let basis = (!watched.out_of_watches()).then(|| Basis::of(root));
            *state = State::Off(basis);
        }
        Ok(delta)
    }

    /// Called for every file a command writes to the tree at `root`: now
    /// and then the events of a watched tree are taken off their queue,
    /// which holds fewer than a checkout of many files causes.
    pub(in crate::native) fn wrote_file(root: &Path) {
        static WRITTEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let written = WRITTEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if !written.is_multiple_of(256) || !super::super::HELD.started() {
            return;
        }
        let tree = {
            let guard = TREES.lock().unwrap_or_else(PoisonError::into_inner);
            guard.as_ref().and_then(|trees| trees.get(root)).cloned()
        };
        if let Some(tree) = tree
            && let State::Watched(watched) = &mut *locked(&tree)
        {
            watched.absorb();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn basis(root: u64, mounts: &str) -> Basis {
            Basis {
                root: Some((1, root)),
                mounts: mounts.as_bytes().to_vec(),
            }
        }

        #[test]
        fn a_tree_is_tried_again_only_on_another_basis_than_it_failed_on() {
            let unreachable = || -> Basis { panic!("the basis is not looked at") };
            assert!(due(&State::New, unreachable));
            assert!(!due(&State::Off(None), unreachable));
            let then = || State::Off(Some(basis(7, "a on /a\n")));
            assert!(!due(&then(), || basis(7, "a on /a\n")));
            // something was mounted or unmounted
            assert!(due(&then(), || basis(7, "")));
            assert!(due(&then(), || basis(7, "a on /a\nb on /b\n")));
            // the root is another directory
            assert!(due(&then(), || basis(8, "a on /a\n")));
            let gone = Basis {
                root: None,
                mounts: b"a on /a\n".to_vec(),
            };
            assert!(due(&then(), || gone));
        }

        #[test]
        fn running_out_of_watches_is_not_tried_again_and_other_failures_are() {
            let failed = |code: i32| off(&std::io::Error::from_raw_os_error(code), basis(7, ""));
            assert!(matches!(failed(28), State::Off(None)));
            assert!(matches!(failed(24), State::Off(None)));
            // no such directory, not supported
            assert!(matches!(failed(2), State::Off(Some(_))));
            assert!(matches!(failed(95), State::Off(Some(_))));
        }

        #[test]
        fn a_tree_whose_lock_a_panic_left_poisoned_is_not_watched_and_stays_usable() {
            let tree: Tree = Arc::new(Mutex::new(State::New));
            let held = tree.clone();
            let panicked = std::thread::spawn(move || {
                let _state = held.lock().unwrap();
                panic!("with the state in hand");
            });
            assert!(panicked.join().is_err());
            assert!(tree.is_poisoned());
            assert!(matches!(*locked(&tree), State::Off(None)));
            // the next command finds a lock like any other, and no watching
            assert!(!tree.is_poisoned());
            assert!(!due(&locked(&tree), || basis(7, "")));
            // another tree is none the worse
            let other: Tree = Arc::new(Mutex::new(State::New));
            assert!(due(&locked(&other), || basis(7, "")));
        }
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
