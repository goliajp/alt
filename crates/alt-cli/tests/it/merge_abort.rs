//! `alt merge --abort`: the index and the files as they were before a merge
//! that stopped, checked against git giving up the same merge: run
//! directly, through the daemon, and by git.

use crate::byte_names::entries;
use crate::cwd_paths::Trees;
use crate::scan_daemon::output;
use crate::stopped_trees::{
    TREES, alike, diverged, every, executable, head, operation, remove, succeeded, working_tree,
};

/// Stops a merge of `side`, runs `changes` and tries to give the merge up.
/// Returns the trees and whether each program gave it up.
fn given_up(changes: impl Fn(&Trees)) -> (Trees, [bool; 3]) {
    let trees = diverged();
    assert_eq!(every(&trees, &["merge", "side"]), [Some(1); 3]);
    changes(&trees);
    let done = succeeded(&trees, &["merge", "--abort"]);
    (trees, done)
}

#[test]
fn abort_puts_the_index_and_the_files_back_as_git_does() {
    let trees = diverged();
    let before: Vec<_> = TREES.iter().map(|tree| head(&trees, tree)).collect();
    let files = working_tree(&trees, "direct");
    assert_eq!(every(&trees, &["merge", "side"]), [Some(1); 3]);
    assert_eq!(operation(&trees), r#"{"kind":"merge"}"#);
    assert_ne!(working_tree(&trees, "direct"), files);

    assert_eq!(every(&trees, &["merge", "--abort"]), [Some(0); 3]);
    alike(&trees, "after the abort");
    assert_eq!(working_tree(&trees, "direct"), files);
    assert_eq!(working_tree(&trees, "served"), files);
    let after: Vec<_> = TREES.iter().map(|tree| head(&trees, tree)).collect();
    assert_eq!(after, before);
    assert_eq!(operation(&trees), "null");
    for tree in ["direct", "served"] {
        assert!(!trees.at(tree).join(".alt/MERGE_HEAD").exists());
        let status = output(&trees.at(tree), &["status"], false);
        let status = String::from_utf8_lossy(&status.stdout).into_owned();
        assert!(status.contains("working tree clean"), "{tree}: {status}");
    }

    // nothing is left of the merge: the branch can be left, and merged again
    assert_eq!(every(&trees, &["switch", "side"]), [Some(0); 3]);
    assert_eq!(every(&trees, &["switch", "main"]), [Some(0); 3]);
    assert_eq!(every(&trees, &["merge", "side"]), [Some(1); 3]);
    assert_eq!(operation(&trees), r#"{"kind":"merge"}"#);
}

#[test]
fn abort_keeps_changes_to_what_the_merge_left_alone_and_untracked_files() {
    let (trees, done) = given_up(|trees| {
        trees.write("u.txt", "changed since\n");
        executable(trees, "keep.sh", false);
        trees.write("untracked.txt", "mine\n");
        trees.write("in/a/dir/mine.txt", "mine\n");
        // what the merge stopped on is given up with it
        trees.write("f.txt", "half resolved\n");
    });
    assert_eq!(done, [true; 3]);
    alike(&trees, "after the abort");
    let files = working_tree(&trees, "direct");
    assert_eq!(files["u.txt"], (b"changed since\n".to_vec(), false));
    assert_eq!(files["keep.sh"], (b"keep.sh\n".to_vec(), false));
    assert_eq!(files["untracked.txt"].0, b"mine\n");
    assert_eq!(files["in/a/dir/mine.txt"].0, b"mine\n");
    assert_eq!(files["f.txt"].0, b"1\nmain\n3\n");
    assert!(!files.contains_key("in/a/dir/n.txt"));
    assert_eq!(operation(&trees), "null");
}

#[test]
fn abort_takes_back_what_was_staged_since_the_merge_stopped() {
    let (trees, done) = given_up(|trees| {
        trees.write("f.txt", "resolved\n");
        trees.write("both.txt", "resolved\n");
        trees.write("u.txt", "staged since\n");
        assert_eq!(
            every(trees, &["add", "f.txt", "both.txt", "u.txt"]),
            [Some(0); 3]
        );
        // a file the merge brought and that is gone again is no change to keep
        remove(trees, "t.txt");
        remove(trees, "new.txt");
    });
    assert_eq!(done, [true; 3]);
    alike(&trees, "after the abort");
    let files = working_tree(&trees, "direct");
    assert_eq!(files["u.txt"].0, b"u.txt\n");
    assert_eq!(files["both.txt"].0, b"main\n");
    assert_eq!(files["t.txt"].0, b"t.txt\n");
}

/// The merge stays as it stopped: the same index and files as in git,
/// which refused as well.
fn still_stopped(trees: &Trees, done: [bool; 3], case: &str) {
    assert_eq!(done, [false; 3], "{case}");
    let theirs = entries(trees, &trees.at("git").join(".git/index"));
    for tree in ["direct", "served"] {
        let ours = entries(trees, &trees.at(tree).join(".alt/index"));
        assert_eq!(
            String::from_utf8_lossy(&ours),
            String::from_utf8_lossy(&theirs),
            "{case}: the index of {tree}"
        );
        assert!(trees.at(tree).join(".alt/MERGE_HEAD").exists(), "{case}");
    }
    assert_eq!(operation(trees), r#"{"kind":"merge"}"#, "{case}");
}

#[test]
fn abort_is_refused_where_it_would_write_over_a_change_that_is_not_staged() {
    // a file the merge changed without a conflict, changed again
    let (trees, done) = given_up(|trees| trees.write("t.txt", "mine\n"));
    still_stopped(&trees, done, "merged and changed");
    for tree in ["direct", "served"] {
        assert_eq!(working_tree(&trees, tree)["t.txt"].0, b"mine\n");
        assert!(working_tree(&trees, tree).contains_key("new.txt"));
    }
    let said = output(&trees.at("direct"), &["merge", "--abort"], false);
    let said = String::from_utf8_lossy(&said.stderr).into_owned();
    assert!(
        said.contains("merge --abort: 't.txt' has changes"),
        "{said}"
    );

    // only its mode changed
    let (trees, done) = given_up(|trees| executable(trees, "new.txt", true));
    still_stopped(&trees, done, "merged and made executable");

    // staged since, and changed after that
    let (trees, done) = given_up(|trees| {
        trees.write("u.txt", "staged\n");
        assert_eq!(every(trees, &["add", "u.txt"]), [Some(0); 3]);
        trees.write("u.txt", "and changed\n");
    });
    still_stopped(&trees, done, "staged and changed");
    assert_eq!(
        working_tree(&trees, "served")["u.txt"].0,
        b"and changed\n".to_vec()
    );

    // a conflict resolved, and changed after that
    let (trees, done) = given_up(|trees| {
        trees.write("f.txt", "resolved\n");
        assert_eq!(every(trees, &["add", "f.txt"]), [Some(0); 3]);
        trees.write("f.txt", "and changed\n");
    });
    still_stopped(&trees, done, "resolved and changed");
}

#[test]
fn abort_is_refused_where_an_untracked_file_is_in_the_way() {
    // the merge deleted `d.txt`; the file there now is not the one it deleted
    let (trees, done) = given_up(|trees| trees.write("d.txt", "mine\n"));
    still_stopped(&trees, done, "an untracked file where one comes back");
    for tree in TREES {
        assert_eq!(working_tree(&trees, tree)["d.txt"].0, b"mine\n", "{tree}");
    }
    // out of the way, the merge is given up
    remove(&trees, "d.txt");
    assert_eq!(every(&trees, &["merge", "--abort"]), [Some(0); 3]);
    alike(&trees, "after the abort");
}

#[test]
fn abort_without_a_merge_is_refused_as_the_abort_of_a_revert_is() {
    let trees = diverged();
    assert_eq!(succeeded(&trees, &["merge", "--abort"]), [false; 3]);
    for (tree, daemon) in [("direct", false), ("served", true)] {
        let merge = output(&trees.at(tree), &["merge", "--abort"], daemon);
        let revert = output(&trees.at(tree), &["revert", "--abort"], daemon);
        assert_eq!(merge.status.code(), revert.status.code(), "{tree}");
        let said = String::from_utf8_lossy(&merge.stderr).into_owned();
        assert!(
            said.contains("fatal: There is no merge to abort (MERGE_HEAD missing)."),
            "{said}"
        );
    }
    alike(&trees, "after the refusal");

    // a branch and `--abort` together are no command, in git as in alt
    assert_eq!(succeeded(&trees, &["merge", "--abort", "side"]), [false; 3]);
    alike(&trees, "after the misuse");
}

#[test]
fn undo_takes_an_abort_back_as_it_takes_the_abort_of_a_revert_back() {
    let unmerged = |trees: &Trees, tree: &str| {
        let status = output(&trees.at(tree), &["status", "--json"], false);
        !String::from_utf8_lossy(&status.stdout).contains(r#""unmerged":[]"#)
    };
    let trees = diverged();
    let at = trees.at("direct");
    assert_eq!(
        output(&at, &["merge", "side"], false).status.code(),
        Some(1)
    );
    let stopped = working_tree(&trees, "direct");
    assert!(output(&at, &["merge", "--abort"], false).status.success());
    assert!(!unmerged(&trees, "direct"));
    assert!(output(&at, &["undo"], false).status.success());
    assert!(unmerged(&trees, "direct"));
    assert_eq!(working_tree(&trees, "direct"), stopped);

    // the same of a revert: its abort is one checkout, which undo inverts
    let at = trees.at("served");
    assert_eq!(
        output(&at, &["revert", "HEAD~1"], false).status.code(),
        Some(1)
    );
    let stopped = working_tree(&trees, "served");
    assert!(output(&at, &["revert", "--abort"], false).status.success());
    assert!(output(&at, &["undo"], false).status.success());
    assert!(unmerged(&trees, "served"));
    assert_eq!(working_tree(&trees, "served"), stopped);
}
