//! After events were lost, a scan lists every directory before the
//! watches are gone over. What is made in a directory between the two, in
//! one that had no watch when it was listed, is in no event and in no
//! listing: it is found all the same.

use std::cell::Cell;
use std::path::Path;

use alt_git_codec::ObjectId;
use alt_worktree::Untracked;

use crate::delta::{fixture, write};
use crate::watch::{Tree, options};
use crate::watch_loss::flood;

/// Scans the watched tree, which has `a.txt` changed: `meanwhile` runs
/// once where the scan reads that file, with every directory listed and
/// no watch gone over yet.
fn scan_with(tree: &mut Tree, meanwhile: impl Fn(&Path)) {
    let root = tree.dir.path().to_path_buf();
    let ran = Cell::new(false);
    let pause = |path: &[u8], _: &[u8], _: ObjectId| {
        if path == b"a.txt" && !ran.replace(true) {
            meanwhile(&root);
        }
        None
    };
    let mut opts = options(Untracked::Normal, &root);
    opts.clean = Some(&pause);
    let scanned = tree
        .watched
        .scan(&root, &mut tree.snapshot, &opts, &[], b"");
    assert!(scanned.is_some());
    assert!(ran.get(), "the scan read the changed file");
}

/// A tree whose queue ran over, with the tracked directory `t/p` removed
/// and made again since, of which there is no event.
fn remade() -> Tree {
    let tree = Tree::new(|root| {
        fixture(root);
        write(root, "t/p/in/f.txt", "f\n");
    });
    let root = tree.dir.path().to_path_buf();
    flood(&root);
    std::fs::remove_dir_all(root.join("t/p")).unwrap();
    std::fs::create_dir(root.join("t/p")).unwrap();
    write(&root, "a.txt", "changed\n");
    tree
}

#[test]
fn a_directory_made_in_one_listed_without_a_watch_is_watched() {
    let mut tree = remade();
    scan_with(&mut tree, |root| {
        std::fs::create_dir(root.join("t/p/in")).unwrap();
    });
    assert_eq!(tree.check().deleted.len(), 1);
    // the file is back as the index has it: seen only with a watch on
    // the directory made in between
    write(tree.root(), "t/p/in/f.txt", "f\n");
    assert!(tree.check().deleted.is_empty());
    write(tree.root(), "t/p/in/deeper/new.txt", "new\n");
    assert_eq!(tree.untracked(), ["t/p/in/deeper/"]);
    std::fs::remove_file(tree.root().join("t/p/in/deeper/new.txt")).unwrap();
    assert!(tree.untracked().is_empty());
    assert_eq!(tree.watched.set_up(), 1);
}

#[test]
fn a_file_made_in_a_directory_listed_without_a_watch_is_seen_next_time() {
    let mut tree = remade();
    scan_with(&mut tree, |root| {
        write(root, "t/p/late.txt", "late\n");
        write(root, "t/p/in/f.txt", "f\n");
    });
    assert_eq!(tree.untracked(), ["t/p/late.txt"]);
    assert!(tree.check().deleted.is_empty());
}
