//! A process that puts another index in place tells the watched tree which
//! index gave way to which. The scan that follows looks at the paths whose
//! entries differ and at what events name, not at the whole tree, and
//! answers what a scan of the whole tree answers.

use std::path::Path;

use alt_git_index::Index;
use alt_worktree::Snapshot;

use crate::delta::{ALGO, fixture, index_of, write};
use crate::watch::Tree;

/// An index holding the working tree as it is now, less `dropped` and
/// what lies beneath those paths.
fn index_less(root: &Path, dropped: &[&str]) -> Index {
    let mut index = index_of(root);
    index.entries.retain(|e| {
        let path = String::from_utf8_lossy(&e.path).into_owned();
        !dropped
            .iter()
            .any(|d| path == *d || path.starts_with(&format!("{d}/")))
    });
    index
}

/// Puts `index` in place as a command of the watching process would.
fn replace(tree: &mut Tree, id: &[u8], index: &Index) {
    let next = Snapshot::of_index(index, id, ALGO, Some(&tree.snapshot));
    tree.watched.index_replaced(&tree.snapshot, &next);
    tree.snapshot = next;
}

#[test]
fn an_index_put_in_place_is_followed_without_a_scan_of_the_whole_tree() {
    let mut tree = Tree::new(fixture);
    let dir = tree.dir.path().to_path_buf();
    let root = dir.as_path();
    let first = index_of(root);
    let whole = tree.watched.scanned_whole();
    write(root, "new/deep/f.txt", "f\n");
    write(root, "new/other.txt", "other\n");
    write(root, "a/more/g.txt", "g\n");
    assert_eq!(tree.untracked(), ["a/more/", "new/"]);
    // the index comes to hold a file in each directory that was one path
    replace(&mut tree, b"two", &index_less(root, &["new/other.txt"]));
    assert_eq!(tree.untracked(), ["new/other.txt"]);
    // and to hold nothing in them again, nor in one it always had files in
    let less = index_less(root, &["new", "a/more", "docs"]);
    replace(&mut tree, b"three", &less);
    assert_eq!(tree.untracked(), ["a/more/", "docs/", "new/"]);
    // an entry takes the content its file has, and gives it up again
    write(root, "a.txt", "changed\n");
    assert_eq!(tree.check().changed.len(), 1);
    replace(
        &mut tree,
        b"four",
        &index_less(root, &["new", "a/more", "docs"]),
    );
    assert!(tree.check().changed.is_empty());
    replace(&mut tree, b"three", &less);
    assert_eq!(tree.check().changed.len(), 1);
    // what happens to the tree around the change of index is not lost
    write(root, "src/lib.rs", "before\n");
    tree.watched.absorb();
    write(root, "run.sh", "between\n");
    replace(&mut tree, b"one", &first);
    write(root, "a0", "after\n");
    let delta = tree.check();
    assert_eq!(delta.changed.len(), 4);
    assert_eq!(
        tree.untracked(),
        ["a/more/", "new/"],
        "docs is in the index again"
    );
    assert_eq!(tree.watched.scanned_whole(), whole);
}

#[test]
fn an_index_that_differs_in_thousands_of_paths_is_followed_by_those_paths() {
    let mut tree = Tree::new(fixture);
    let dir = tree.dir.path().to_path_buf();
    let root = dir.as_path();
    let first = index_of(root);
    for f in 0..3000 {
        write(
            root,
            &format!("many/d{}/e{}/f{f}.txt", f % 30, f % 7),
            "x\n",
        );
    }
    assert_eq!(tree.untracked(), ["many/"]);
    let whole = tree.watched.scanned_whole();
    // all of them but one directory's
    let most = index_less(root, &["many/d7"]);
    replace(&mut tree, b"two", &most);
    assert_eq!(tree.untracked(), ["many/d7/"]);
    write(root, "many/d3/e3/f3.txt", "changed\n");
    std::fs::remove_file(root.join("many/d4/e4/f4.txt")).unwrap();
    let delta = tree.check();
    assert_eq!((delta.changed.len(), delta.deleted.len()), (1, 1));
    replace(&mut tree, b"one", &first);
    assert_eq!(tree.untracked(), ["many/"]);
    assert_eq!(tree.watched.scanned_whole(), whole);
    // an index the tree was not told about is answered by a scan of everything
    tree.snapshot = Snapshot::of_index(&most, b"two", ALGO, Some(&tree.snapshot));
    assert_eq!(tree.untracked(), ["many/d7/"]);
    assert_eq!(tree.watched.scanned_whole(), whole + 1);
}
