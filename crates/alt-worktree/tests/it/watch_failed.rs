//! A scan of a watched tree that fails says so and nothing more: the
//! tree is watched as before. Watching ends only where the watches
//! themselves cannot be kept.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use alt_worktree::{Unanswered, Untracked, scan};

use crate::delta::{fixture, write};
use crate::watch::{Tree, options};

unsafe extern "C" {
    /// Sets the user that access to files is checked for, on the calling
    /// thread alone; returns the one it was.
    fn setfsuid(uid: u32) -> i32;
}

/// Runs `f` as a user the mode of a file holds for. The tests may run as
/// root, whom no mode keeps out.
fn bound_by_modes<T>(f: impl FnOnce() -> T) -> T {
    const NOBODY: u32 = 65534;
    // SAFETY: no pointers are passed
    let was = unsafe { setfsuid(NOBODY) };
    let done = f();
    // SAFETY: no pointers are passed
    unsafe { setfsuid(was as u32) };
    done
}

fn mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn a_scan_that_fails_leaves_the_tree_watched() {
    let mut tree = Tree::new(fixture);
    let root = tree.dir.path().to_path_buf();
    mode(&root, 0o755);
    mode(&root.join("src/deep"), 0o000);
    write(&root, "a.txt", "changed\n");
    // the scan of the paths with events, then one of everything
    for round in 0..2 {
        let opts = options(Untracked::Normal, &root);
        let (watched, direct) = bound_by_modes(|| {
            let watched = tree
                .watched
                .scan(&root, &mut tree.snapshot, &opts, &[], b"");
            (watched, scan(&root, &mut tree.snapshot.clone(), &opts))
        });
        assert_eq!(watched, Err(Unanswered::ScanFailed), "round {round}");
        assert!(direct.is_err(), "round {round}");
    }
    mode(&root.join("src/deep"), 0o755);
    assert_eq!(tree.check().changed.len(), 1);
    write(
        &root,
        "src/deep/er/mod.rs",
        "changed where the scan failed\n",
    );
    write(&root, "src/deep/new/file.txt", "new\n");
    assert_eq!(tree.check().changed.len(), 2);
    assert_eq!(tree.untracked(), ["src/deep/new/"]);
    assert_eq!(tree.watched.set_up(), 1);
}

#[test]
fn a_scan_that_fails_after_events_were_lost_makes_up_for_them_next_time() {
    let mut tree = Tree::new(fixture);
    let root = tree.dir.path().to_path_buf();
    mode(&root, 0o755);
    crate::watch_loss::flood(&root);
    // none of this is in an event
    write(&root, "made/in/between.txt", "new\n");
    mode(&root.join("src/deep"), 0o000);
    let opts = options(Untracked::Normal, &root);
    let watched = bound_by_modes(|| {
        tree.watched
            .scan(&root, &mut tree.snapshot, &opts, &[], b"")
    });
    assert_eq!(watched, Err(Unanswered::ScanFailed));
    mode(&root.join("src/deep"), 0o755);
    assert_eq!(tree.untracked(), ["made/"]);
    // the directory made while events were lost has its watch
    std::fs::remove_file(root.join("made/in/between.txt")).unwrap();
    assert!(tree.untracked().is_empty());
    assert_eq!(tree.watched.set_up(), 1);
}
