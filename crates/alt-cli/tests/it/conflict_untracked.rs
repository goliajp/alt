//! A merge, pick or revert that stops in conflict writes over no untracked
//! file: where one is in the way it is refused before anything is written,
//! as git refuses it, and a file it has no business with stays.

use crate::byte_names::entries;
use crate::cwd_paths::Trees;
use crate::stopped_trees::{
    TREES, alike, commit, every, operation, remove, succeeded, working_tree,
};

/// `side` and `main` both change `f.txt` and both delete `e.txt`; `side`
/// adds `new.txt` and deletes `d.txt`, and changes `f.txt` once more.
fn diverged() -> Trees {
    let trees = Trees::new();
    trees.write("f.txt", "1\n2\n3\n");
    trees.write("d.txt", "d\n");
    trees.write("e.txt", "e\n");
    commit(&trees, "files");
    assert_eq!(every(&trees, &["switch", "-c", "side"]), [Some(0); 3]);
    trees.write("f.txt", "1\nside\n3\n");
    trees.write("new.txt", "new\n");
    remove(&trees, "d.txt");
    remove(&trees, "e.txt");
    commit(&trees, "side");
    trees.write("f.txt", "1\nside again\n3\n");
    commit(&trees, "side again");
    assert_eq!(every(&trees, &["switch", "main"]), [Some(0); 3]);
    trees.write("f.txt", "1\nmain\n3\n");
    remove(&trees, "e.txt");
    commit(&trees, "main");
    trees
}

/// Nothing was written: the index and the files are those of git, which
/// refused as well, the file in the way is there, and no operation began.
fn refused(trees: &Trees, command: &[&str], file: &str) {
    let before: Vec<_> = TREES.iter().map(|tree| working_tree(trees, tree)).collect();
    assert_eq!(succeeded(trees, command), [false; 3], "{command:?}");
    let after: Vec<_> = TREES.iter().map(|tree| working_tree(trees, tree)).collect();
    assert_eq!(after, before, "{command:?}");
    assert_eq!(after[0][file].0, b"mine\n");
    alike(trees, "after the refusal");
    assert_eq!(operation(trees), "null", "{command:?}");
    let said = crate::scan_daemon::output(&trees.at("direct"), command, false);
    let said = String::from_utf8_lossy(&said.stderr).into_owned();
    let why = format!("untracked working tree file '{file}' would be overwritten");
    assert!(said.contains(&why), "{said}");
}

#[test]
fn a_merge_or_a_pick_that_would_stop_is_refused_over_an_untracked_file() {
    let trees = diverged();
    trees.write("new.txt", "mine\n");
    refused(&trees, &["merge", "side"], "new.txt");
    refused(&trees, &["cherry-pick", "side~1"], "new.txt");
    // there is no merge to give up, and the file is no part of one
    assert_eq!(succeeded(&trees, &["merge", "--abort"]), [false; 3]);
    assert_eq!(working_tree(&trees, "served")["new.txt"].0, b"mine\n");

    // out of the way, the merge stops as in git
    remove(&trees, "new.txt");
    assert_eq!(every(&trees, &["merge", "side"]), [Some(1); 3]);
    let theirs = entries(&trees, &trees.at("git").join(".git/index"));
    let ours = entries(&trees, &trees.at("direct").join(".alt/index"));
    assert_eq!(ours, theirs);
}

#[test]
fn a_revert_that_would_stop_is_refused_over_an_untracked_file() {
    let trees = diverged();
    assert_eq!(every(&trees, &["switch", "side"]), [Some(0); 3]);
    // reverting `side` brings d.txt back, and stops in f.txt
    trees.write("d.txt", "mine\n");
    refused(&trees, &["revert", "--no-edit", "HEAD~1"], "d.txt");
}

#[test]
fn an_untracked_file_where_both_sides_deleted_one_stays_through_merge_and_abort() {
    let trees = diverged();
    trees.write("e.txt", "mine\n");
    assert_eq!(every(&trees, &["merge", "side"]), [Some(1); 3]);
    for tree in TREES {
        assert_eq!(working_tree(&trees, tree)["e.txt"].0, b"mine\n", "{tree}");
    }
    assert_eq!(every(&trees, &["merge", "--abort"]), [Some(0); 3]);
    alike(&trees, "after the abort");
    assert_eq!(working_tree(&trees, "direct")["e.txt"].0, b"mine\n");
}
