//! The working trees a process that serves many commands watches for
//! changes, so that a scan looks only where something happened.

#[cfg(not(target_os = "linux"))]
pub(in crate::native) use unwatched::wrote_file;
#[cfg(not(target_os = "linux"))]
pub(super) use unwatched::{forget, index_replaced, watched_scan, watches};

#[cfg(target_os = "linux")]
pub(in crate::native) use linux::wrote_file;
#[cfg(target_os = "linux")]
pub(super) use linux::{forget, index_replaced, watched_scan, watches};

#[cfg(not(target_os = "linux"))]
mod unwatched {
    use std::path::Path;

    use alt_worktree::{Delta, ScanOptions, Snapshot};

    use super::super::Res;

    pub(in crate::native) fn wrote_file(_: &Path) {}

    pub(in crate::native::tree_state) fn watches(_: &Path) -> bool {
        false
    }

    pub(in crate::native::tree_state) fn forget(_: &Path) {}

    pub(in crate::native::tree_state) fn index_replaced(
        _: &Path,
        _: &Snapshot,
        _: &mut Snapshot,
        _: &ScanOptions<'_>,
        _: (&[&Path], &[u8]),
    ) {
    }

    pub(in crate::native::tree_state) fn watched_scan(
        _: &Path,
        _: &mut Snapshot,
        _: &ScanOptions<'_>,
        _: (&[&Path], &[u8]),
    ) -> Res<Option<Delta>> {
        Ok(None)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

    use alt_worktree::{Delta, ScanOptions, Snapshot, Unanswered, Watched};

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
            Ok(Some(watched)) => State::Watched(Box::new(watched)),
            // an ignore file could not be read: the next command tries
            Ok(None) => State::New,
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
        match super::super::HELD.started() {
            true => Ok(scan_tree(root, snapshot, options, settings)),
            false => Ok(None),
        }
    }

    /// Scans the tree at `root` under its own lock, watching it from now
    /// on when that is due.
    fn scan_tree(
        root: &Path,
        snapshot: &mut Snapshot,
        options: &ScanOptions<'_>,
        settings: (&[&Path], &[u8]),
    ) -> Option<Delta> {
        let tree = tree(root);
        let mut state = locked(&tree);
        if due(&state, || Basis::of(root)) {
            *state = start(root, snapshot, options);
        }
        answer(&mut state, root, snapshot, options, settings)
    }

    /// What a tree in `state` answers when it is watched. One that can
    /// no longer be watched is not from then on; one whose scan failed
    /// stays watched.
    fn answer(
        state: &mut State,
        root: &Path,
        snapshot: &mut Snapshot,
        options: &ScanOptions<'_>,
        settings: (&[&Path], &[u8]),
    ) -> Option<Delta> {
        let State::Watched(watched) = state else {
            return None;
        };
        match watched.scan(root, snapshot, options, settings.0, settings.1) {
            Ok(delta) => Some(delta),
            Err(Unanswered::ScanFailed) => None,
            Err(Unanswered::Unwatched) => {
                let basis = (!watched.out_of_watches()).then(|| Basis::of(root));
                *state = State::Off(basis);
                None
            }
        }
    }

    /// The tree at `root`, when this process has scanned it before.
    fn known(root: &Path) -> Option<Tree> {
        let guard = TREES.lock().unwrap_or_else(PoisonError::into_inner);
        guard.as_ref().and_then(|trees| trees.get(root)).cloned()
    }

    /// Whether the tree at `root` is watched.
    pub(in crate::native::tree_state) fn watches(root: &Path) -> bool {
        known(root).is_some_and(|tree| matches!(*locked(&tree), State::Watched(_)))
    }

    /// For the watched tree at `root`, whose index this process replaced:
    /// the index of `snapshot` stands where that of `prior` stood. The
    /// tree is scanned where the two differ and where events name a path,
    /// and holds the answer for the command that asks next.
    pub(in crate::native::tree_state) fn index_replaced(
        root: &Path,
        prior: &Snapshot,
        snapshot: &mut Snapshot,
        options: &ScanOptions<'_>,
        settings: (&[&Path], &[u8]),
    ) {
        let Some(tree) = known(root) else {
            return;
        };
        let mut state = locked(&tree);
        if let State::Watched(watched) = &mut *state {
            watched.index_replaced(prior, snapshot);
        }
        answer(&mut state, root, snapshot, options, settings);
    }

    /// Drops the answer held for the tree at `root`.
    pub(in crate::native::tree_state) fn forget(root: &Path) {
        if let Some(tree) = known(root)
            && let State::Watched(watched) = &mut *locked(&tree)
        {
            watched.forget();
        }
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
        if let Some(tree) = known(root)
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

        /// A tree with one file, which does not hold what the snapshot
        /// returned says it holds: a scan reads it.
        fn changed_tree() -> (alt_os::temp::TempDir, Snapshot) {
            use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
            let dir = alt_os::temp::tempdir().unwrap();
            std::fs::create_dir(dir.path().join(".alt")).unwrap();
            std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
            let entry = alt_git_index::IndexEntry {
                ctime: (0, 0),
                mtime: (0, 0),
                dev: 0,
                ino: 0,
                mode: 0o100644,
                uid: 0,
                gid: 0,
                size: 0,
                oid: ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, b"staged\n"),
                flags: 5,
                extended_flags: None,
                path: "a.txt".into(),
            };
            let index = alt_git_index::Index {
                version: 2,
                entries: vec![entry],
                extensions: Vec::new(),
            };
            let snapshot = Snapshot::of_index(&index, b"index", HashAlgo::Sha1, None);
            (dir, snapshot)
        }

        fn options<'a>(
            root: &Path,
            clean: Option<alt_worktree::CleanTracked<'a>>,
        ) -> ScanOptions<'a> {
            ScanOptions {
                untracked: alt_worktree::Untracked::Normal,
                within: &[],
                clean,
                head: None,
                excludes: &[],
                clock: alt_worktree::Clock::read(&root.join(".alt")).unwrap(),
            }
        }

        #[test]
        fn a_tree_whose_scan_failed_is_watched_as_before() {
            use std::os::unix::fs::PermissionsExt;
            unsafe extern "C" {
                /// Sets the user that access to files is checked for, on
                /// the calling thread alone; returns the one it was.
                fn setfsuid(uid: u32) -> i32;
            }
            let mode = |path: &Path, mode: u32| {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
            };
            let (dir, mut snapshot) = changed_tree();
            let root = dir.path();
            let mut scan = |as_user: Option<u32>| {
                let options = options(root, None);
                // SAFETY: no pointers are passed
                let was = as_user.map(|user| unsafe { setfsuid(user) });
                let delta = scan_tree(root, &mut snapshot, &options, (&[], b""));
                // SAFETY: no pointers are passed
                was.map(|was| unsafe { setfsuid(was as u32) });
                delta
            };
            assert!(scan(None).is_some());
            mode(root, 0o755);
            let closed = root.join("closed");
            std::fs::create_dir(&closed).unwrap();
            std::fs::write(closed.join("file"), "new\n").unwrap();
            assert_eq!(scan(None).unwrap().untracked.len(), 1);
            // as a user whom the mode keeps out, which it does not keep root
            mode(&closed, 0o000);
            assert!(scan(Some(65534)).is_none());
            assert!(watches(root), "a scan that failed ends no watching");
            mode(&closed, 0o755);
            assert_eq!(scan(None).unwrap().untracked.len(), 1);
        }

        #[test]
        fn a_scan_of_one_tree_keeps_no_scan_of_another_waiting() {
            use std::sync::mpsc::channel;
            use std::time::Duration;
            let (one, mut first) = changed_tree();
            let (other, mut second) = changed_tree();
            let (began, has_begun) = channel();
            let (go, may_go) = channel::<()>();
            let (answered, has_answered) = channel();
            std::thread::scope(|scope| {
                // the scan of the first tree stops where it reads the file
                let (root, first) = (one.path(), &mut first);
                let slow = scope.spawn(move || {
                    let wait = |_: &[u8], _: &[u8], _: alt_git_codec::ObjectId| {
                        began.send(()).unwrap();
                        may_go.recv().unwrap();
                        None
                    };
                    let options = options(root, Some(&wait));
                    scan_tree(root, first, &options, (&[], b""))
                });
                has_begun.recv_timeout(Duration::from_secs(10)).unwrap();
                scope.spawn(|| {
                    let options = options(other.path(), None);
                    let delta = scan_tree(other.path(), &mut second, &options, (&[], b""));
                    answered.send(delta).unwrap();
                });
                let meanwhile = has_answered.recv_timeout(Duration::from_secs(10));
                go.send(()).unwrap();
                let delta = meanwhile.expect("the other tree is scanned while the first is");
                assert_eq!(delta.expect("it is watched").changed.len(), 1);
                assert_eq!(slow.join().unwrap().unwrap().changed.len(), 1);
            });
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
