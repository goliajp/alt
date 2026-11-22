//! A step that stopped on a file both sides added, given up or skipped:
//! the file is put back like any other, as git puts it back.

use crate::byte_names::entries;
use crate::cwd_paths::Trees;
use crate::stopped_trees::{alike, commit, every, remove, succeeded, working_tree};

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

#[test]
fn aborting_a_revert_is_refused_where_it_would_write_over_what_is_not_staged() {
    let trees = Trees::new();
    trees.write("c.txt", "c1\n");
    trees.write("r.txt", "r1\n");
    commit(&trees, "first");
    trees.write("c.txt", "c2\n");
    trees.write("r.txt", "r2\n");
    commit(&trees, "second");
    trees.write("c.txt", "c3\n");
    commit(&trees, "third");

    // stops in c.txt; r.txt goes back to r1 and is staged
    let revert = ["revert", "--no-edit", "HEAD~1"];
    assert_eq!(every(&trees, &revert), [Some(1); 3]);
    trees.write("r.txt", "mine\n");
    assert_eq!(succeeded(&trees, &["revert", "--abort"]), [false; 3]);
    let theirs = entries(&trees, &trees.at("git").join(".git/index"));
    for tree in ["direct", "served"] {
        let ours = entries(&trees, &trees.at(tree).join(".alt/index"));
        assert_eq!(ours, theirs, "the index of {tree} after the refusal");
        assert_eq!(working_tree(&trees, tree)["r.txt"].0, b"mine\n");
    }
    let said = crate::scan_daemon::output(&trees.at("direct"), &["revert", "--abort"], false);
    let said = String::from_utf8_lossy(&said.stderr).into_owned();
    assert!(
        said.contains("revert --abort: 'r.txt' has changes"),
        "{said}"
    );

    // a change to a file the revert left alone is kept, as in git
    trees.write("r.txt", "r1\n");
    trees.write("top.txt", "mine\n");
    // git goes by the stat of r.txt until a status has read the file again
    assert_eq!(every(&trees, &["status"]), [Some(0); 3]);
    assert_eq!(every(&trees, &["revert", "--abort"]), [Some(0); 3]);
    alike(&trees, "after the abort");
    assert_eq!(working_tree(&trees, "served")["top.txt"].0, b"mine\n");
}
