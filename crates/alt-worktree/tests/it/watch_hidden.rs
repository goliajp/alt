//! A directory an ignore rule hides, with nothing tracked in it, takes no
//! watch, and is followed all the same once a rule or the index brings
//! it back, or it is renamed or made again.

use std::path::Path;

use alt_worktree::Snapshot;

use crate::delta::{ALGO, fixture, index_of, write};
use crate::watch::{ENOSPC, Tree, start};

fn build_output(root: &Path) {
    write(root, "target/debug/deps/lib.o", "o\n");
    write(root, "target/debug/build/out", "out\n");
}

#[test]
fn a_directory_an_ignore_rule_hides_takes_no_watches() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    // the root, a, docs, src, src/deep, src/deep/er, and four of `target`
    fixture(root);
    build_output(root);
    let all = Snapshot::of_index(&index_of(root), b"all", ALGO, None);
    write(root, ".gitignore", "/target/\n");
    let fixture_only = Snapshot::of_index(&index_of(root), b"some", ALGO, None);
    let refused = start(root, 5, &fixture_only).err().unwrap();
    assert_eq!(refused.raw_os_error(), Some(ENOSPC));
    assert!(start(root, 6, &fixture_only).is_ok());
    // with files of it in the index it is looked into, and watched
    let refused = start(root, 9, &all).err().unwrap();
    assert_eq!(refused.raw_os_error(), Some(ENOSPC));
    assert!(start(root, 10, &all).is_ok());
}

#[test]
fn a_hidden_directory_is_watched_once_a_rule_or_the_index_brings_it_back() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    write(root, "other/deep/kept.txt", "kept\n");
    let full = index_of(root);
    let mut fewer = full.clone();
    fewer
        .entries
        .retain(|e| !e.path.to_string().starts_with("other/"));
    write(root, ".gitignore", "/target/\n/other/\n");
    build_output(root);
    std::fs::create_dir(root.join(".alt")).unwrap();
    let snapshot = Snapshot::of_index(&fewer, b"one", ALGO, None);
    let watched = start(root, usize::MAX, &snapshot).unwrap();
    let mut tree = Tree {
        dir,
        snapshot,
        watched,
    };
    assert!(tree.untracked().is_empty());
    write(tree.root(), "target/debug/deps/more.o", "o\n");
    assert!(tree.untracked().is_empty());

    // the rule goes: the directory shows, and what happens in it is seen
    write(tree.root(), ".gitignore", "/other/\n");
    assert_eq!(tree.untracked(), ["target/"]);
    std::fs::remove_dir_all(tree.root().join("target/debug")).unwrap();
    assert!(tree.untracked().is_empty());
    write(tree.root(), "target/debug/deps/again.o", "o\n");
    assert_eq!(tree.untracked(), ["target/"]);

    // a file of the other directory enters the index: changes to it are
    // seen like those of any other file
    tree.snapshot = Snapshot::of_index(&full, b"two", ALGO, Some(&tree.snapshot));
    // the ignore file is the one path that differs
    let delta = tree.check();
    assert_eq!((delta.changed.len(), delta.deleted.len()), (1, 0));
    write(
        tree.root(),
        "other/deep/kept.txt",
        "changed where nothing was watched\n",
    );
    assert_eq!(tree.check().changed.len(), 2);
    std::fs::remove_file(tree.root().join("other/deep/kept.txt")).unwrap();
    assert_eq!(tree.check().deleted.len(), 1);
}

#[test]
fn a_hidden_directory_that_is_renamed_or_made_again_is_followed() {
    let mut tree = Tree::new(fixture);
    write(tree.root(), ".gitignore", "build/\n");
    write(tree.root(), "src/build/a/b/out", "out\n");
    assert!(tree.untracked().is_empty());
    // under a name no rule hides
    std::fs::rename(tree.root().join("src/build"), tree.root().join("src/built")).unwrap();
    assert_eq!(tree.untracked(), ["src/built/"]);
    std::fs::remove_file(tree.root().join("src/built/a/b/out")).unwrap();
    assert!(tree.untracked().is_empty());
    write(tree.root(), "src/built/a/b/out", "out\n");
    std::fs::rename(tree.root().join("src/built"), tree.root().join("src/build")).unwrap();
    assert!(tree.untracked().is_empty());
    std::fs::remove_dir_all(tree.root().join("src/build")).unwrap();
    write(
        tree.root(),
        "src/build",
        "a file by the name of the directory\n",
    );
    assert_eq!(tree.untracked(), ["src/build"]);
}
