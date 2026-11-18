//! A scan of a watched tree that fails says so and nothing more: the
//! tree is watched as before. Watching ends only where the watches
//! themselves cannot be kept.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use alt_worktree::{Snapshot, Unanswered, Untracked, scan};

use crate::delta::{ALGO, fixture, index_of, write};
use crate::watch::{Tree, options, start};

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

#[test]
fn events_taken_off_the_queue_while_a_directory_is_renamed_leave_hidden_ones_unwatched() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    // the root, a, docs, src, src/deep, src/deep/er: six watches
    fixture(root);
    let snapshot = Snapshot::of_index(&index_of(root), b"one", ALGO, None);
    write(
        root,
        "build/out/a/b/c/d/o.o",
        "hidden by the rule on build\n",
    );
    std::fs::create_dir(root.join(".alt")).unwrap();
    let watched = start(root, 8, &snapshot).unwrap();
    let mut tree = Tree {
        dir,
        snapshot,
        watched,
    };
    tree.check();
    // which watch is on which path is no longer known: all are set anew
    std::fs::rename(tree.root().join("docs"), tree.root().join("papers")).unwrap();
    write(tree.root(), "build/more/of/it/o.o", "hidden as well\n");
    tree.watched.absorb();
    assert!(!tree.watched.out_of_watches());
    assert_eq!(tree.untracked(), ["papers/"]);
    assert_eq!(tree.check().deleted.len(), 2);
    assert!(!tree.watched.out_of_watches());
}

/// What a watched tree answers as a user the modes hold for.
fn bound_scan(tree: &mut Tree) -> Result<alt_worktree::Delta, Unanswered> {
    let root = tree.dir.path().to_path_buf();
    let opts = options(Untracked::Normal, &root);
    bound_by_modes(|| {
        tree.watched
            .scan(&root, &mut tree.snapshot, &opts, &[], b"")
    })
}

#[test]
fn a_directory_made_that_cannot_be_read_leaves_the_tree_watched() {
    let mut tree = Tree::new(fixture);
    let root = tree.dir.path().to_path_buf();
    mode(&root, 0o755);
    std::fs::create_dir(root.join("locked")).unwrap();
    mode(&root.join("locked"), 0o000);
    for round in 0..2 {
        assert_ne!(
            bound_scan(&mut tree),
            Err(Unanswered::Unwatched),
            "round {round}"
        );
    }
    mode(&root.join("locked"), 0o755);
    tree.check();
    // the directory has its watch, and one made in it has
    write(&root, "locked/in/file.txt", "new\n");
    assert_eq!(tree.untracked(), ["locked/"]);
    std::fs::remove_file(root.join("locked/in/file.txt")).unwrap();
    assert!(tree.untracked().is_empty());
    assert_eq!(tree.watched.set_up(), 1);
}

#[test]
fn an_ignore_file_that_cannot_be_read_leaves_the_tree_watched() {
    let mut tree = Tree::new(fixture);
    let root = tree.dir.path().to_path_buf();
    mode(&root, 0o755);
    write(&root, "docs/.gitignore", "*.tmp\n");
    mode(&root.join("docs/.gitignore"), 0o000);
    for round in 0..2 {
        assert_ne!(
            bound_scan(&mut tree),
            Err(Unanswered::Unwatched),
            "round {round}"
        );
    }
    mode(&root.join("docs/.gitignore"), 0o644);
    assert_eq!(tree.untracked(), ["docs/.gitignore"]);
    write(&root, "docs/x.tmp", "hidden\n");
    write(&root, "docs/more/file.txt", "new\n");
    assert_eq!(tree.untracked(), ["docs/.gitignore", "docs/more/"]);
    assert_eq!(tree.watched.set_up(), 1);
}

#[test]
fn a_tree_first_watched_with_a_directory_that_cannot_be_read_is_watched() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fixture(&root);
    mode(&root, 0o755);
    let snapshot = Snapshot::of_index(&index_of(&root), b"one", ALGO, None);
    std::fs::create_dir(root.join(".alt")).unwrap();
    std::fs::create_dir(root.join("locked")).unwrap();
    mode(&root.join("locked"), 0o000);
    let opts = options(Untracked::Normal, &root);
    let watched =
        bound_by_modes(|| alt_worktree::Watched::start(&root, usize::MAX, &snapshot, &opts))
            .unwrap()
            .unwrap();
    let mut tree = Tree {
        dir,
        snapshot,
        watched,
    };
    assert_ne!(bound_scan(&mut tree), Err(Unanswered::Unwatched));
    mode(&root.join("locked"), 0o755);
    tree.check();
    write(&root, "locked/file.txt", "new\n");
    assert_eq!(tree.untracked(), ["locked/"]);
    assert_eq!(tree.watched.set_up(), 1);
}
