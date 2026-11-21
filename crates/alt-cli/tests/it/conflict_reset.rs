//! A step that stopped on a file both sides added, given up or skipped:
//! the file is put back like any other, as git puts it back.

use crate::cwd_paths::Trees;
use crate::stopped_trees::{alike, commit, every, remove, working_tree};

#[test]
fn aborting_a_revert_clears_a_file_both_sides_added() {
    let trees = Trees::new();
    trees.write("f.txt", "first\n");
    commit(&trees, "add f");
    remove(&trees, "f.txt");
    commit(&trees, "delete f");
    trees.write("f.txt", "second\n");
    commit(&trees, "add f again");

    // the revert brings back a file that is there again with other content
    assert_eq!(
        every(&trees, &["revert", "--no-edit", "HEAD~1"]),
        [Some(1); 3]
    );
    assert_eq!(every(&trees, &["revert", "--abort"]), [Some(0); 3]);
    alike(&trees, "after the abort");
    assert_eq!(working_tree(&trees, "direct")["f.txt"].0, b"second\n");
}

#[test]
fn skipping_a_rebase_step_clears_a_file_both_sides_added() {
    let trees = Trees::new();
    assert_eq!(every(&trees, &["switch", "-c", "side"]), [Some(0); 3]);
    trees.write("f.txt", "side\n");
    commit(&trees, "side adds f");
    trees.write("g.txt", "g\n");
    commit(&trees, "side adds g");
    assert_eq!(every(&trees, &["switch", "main"]), [Some(0); 3]);
    trees.write("f.txt", "main\n");
    commit(&trees, "main adds f");
    assert_eq!(every(&trees, &["switch", "side"]), [Some(0); 3]);

    assert_eq!(every(&trees, &["rebase", "main"]), [Some(1); 3]);
    assert_eq!(every(&trees, &["rebase", "--skip"]), [Some(0); 3]);
    alike(&trees, "after the skip");
    let files = working_tree(&trees, "served");
    assert_eq!(files["f.txt"].0, b"main\n");
    assert_eq!(files["g.txt"].0, b"g\n");
}
