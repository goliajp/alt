//! When the queue of a watcher runs over, events are missing. The scan
//! that follows makes up for them, and the watches stay: only directories
//! that came to be in the meantime are given one.

use std::path::Path;

use crate::delta::{fixture, write};
use crate::watch::Tree;

/// Causes more events than the queue holds, 16384 unless raised, in a
/// directory that is watched; whatever happens after that leaves no event
/// until the queue is read.
fn flood(root: &Path) {
    for f in 0..9000 {
        write(root, &format!("src/flood{f}.txt"), "x\n");
    }
    for f in 0..9000 {
        std::fs::remove_file(root.join(format!("src/flood{f}.txt"))).unwrap();
    }
}

#[test]
fn a_queue_that_ran_over_keeps_its_watches_and_gains_the_missing_ones() {
    let mut tree = Tree::new(|root| {
        fixture(root);
        write(root, "other/deep/kept.txt", "kept\n");
    });
    let dir = tree.dir.path().to_path_buf();
    let root = dir.as_path();
    flood(root);
    // none of this is in an event
    write(root, "a.txt", "changed\n");
    write(root, "new/a/b/f.txt", "f\n");
    write(root, "new/c/g.txt", "g\n");
    write(root, "src/fresh/x.rs", "x\n");
    write(root, "build/out/o.o", "o\n");
    write(root, "empty/within/.keep.log", "hidden\n");
    std::fs::rename(root.join("docs"), root.join("papers")).unwrap();
    std::fs::rename(root.join("other/deep"), root.join("other/moved")).unwrap();
    std::fs::remove_dir_all(root.join("a")).unwrap();
    let untracked = tree.untracked();
    for shown in ["new/", "src/fresh/", "papers/", "other/moved/"] {
        assert!(
            untracked.contains(&shown.to_owned()),
            "{shown}: {untracked:?}"
        );
    }
    assert_eq!(tree.watched.set_up(), 1, "the watches were set up anew");

    // what happens from here on in directories old and new is followed
    write(root, "src/lib.rs", "in a directory watched all along\n");
    assert_eq!(tree.check().changed.len(), 2);
    std::fs::remove_file(root.join("new/a/b/f.txt")).unwrap();
    assert!(tree.untracked().contains(&"new/".to_owned()));
    std::fs::remove_file(root.join("new/c/g.txt")).unwrap();
    assert!(!tree.untracked().contains(&"new/".to_owned()));
    write(root, "new/a/b/again.txt", "again\n");
    assert!(tree.untracked().contains(&"new/".to_owned()));
    std::fs::remove_file(root.join("src/fresh/x.rs")).unwrap();
    assert!(!tree.untracked().contains(&"src/fresh/".to_owned()));
    write(root, "src/fresh/deeper/y.rs", "y\n");
    assert!(tree.untracked().contains(&"src/fresh/".to_owned()));
    // a directory that moved took its watch along
    write(root, "papers/guide.md", "edited where it went\n");
    write(root, "other/moved/kept.txt", "edited where it went\n");
    write(root, "docs/guide.md", "a new one where it was\n");
    write(root, "other/deep/kept.txt", "a new one where it was\n");
    write(root, "a/x.txt", "made again\n");
    let delta = tree.check();
    assert_eq!(delta.changed.len(), 5);
    std::fs::remove_file(root.join("papers/guide.md")).unwrap();
    std::fs::remove_file(root.join("papers/old.md")).unwrap();
    std::fs::remove_file(root.join("other/moved/kept.txt")).unwrap();
    let untracked = tree.untracked();
    assert!(!untracked.contains(&"papers/".to_owned()), "{untracked:?}");
    assert!(
        !untracked.contains(&"other/moved/".to_owned()),
        "{untracked:?}"
    );
    // what a rule hid is watched once the rule is gone
    write(root, "empty/within/shown.txt", "shown\n");
    assert!(tree.untracked().contains(&"empty/".to_owned()));
    write(root, ".gitignore", "*.log\n");
    assert!(tree.untracked().contains(&"build/".to_owned()));
    std::fs::remove_file(root.join("build/out/o.o")).unwrap();
    assert!(!tree.untracked().contains(&"build/".to_owned()));
    assert_eq!(tree.watched.set_up(), 1);

    // and once more, with the index changed as well
    flood(root);
    write(root, "late/f.txt", "f\n");
    tree.snapshot.index_id = b"two".to_vec();
    assert!(tree.untracked().contains(&"late/".to_owned()));
    std::fs::remove_file(root.join("late/f.txt")).unwrap();
    assert!(!tree.untracked().contains(&"late/".to_owned()));
    assert_eq!(tree.watched.set_up(), 1);
}
