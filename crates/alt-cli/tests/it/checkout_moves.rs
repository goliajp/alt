//! A checkout that moves every file of a directory to another renames
//! the directory only where that leaves nothing behind: what is no file
//! of the checkout stays where git leaves it. A `mv` of the directory
//! takes all of it, as git's does.

use crate::casefold::spelled;
use crate::like_git::{Pair, alt, ok};

/// `d` with files at two depths on `main`, and `moved` where it is `e`.
fn moved() -> Pair {
    let pair = Pair::new();
    pair.write("d/x", "x\n");
    pair.write("d/s/y", "y\n");
    pair.write("keep", "k\n");
    pair.write(".gitignore", "*.ign\n");
    pair.commit_all("base");
    pair.both(&["switch", "-c", "moved"]);
    pair.both(&["mv", "d", "e"]);
    pair.commit_all("moved");
    pair.both(&["switch", "main"]);
    pair
}

/// Every name in both trees is the same, directories among them.
fn same_names(pair: &Pair, when: &str) {
    assert_eq!(spelled(&pair.alt), spelled(&pair.git), "{when}");
}

fn make(pair: &Pair, dirs: &[&str]) {
    for root in [&pair.alt, &pair.git] {
        for dir in dirs {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
    }
}

#[test]
fn a_directory_that_moves_whole_leaves_an_empty_directory_where_git_leaves_it() {
    let pair = moved();
    // directories no file is in, which are nothing the checkout tracks
    make(&pair, &["d/empty", "d/s/hollow", "d/deep/er"]);
    pair.both(&["switch", "moved"]);
    pair.assert_same_state();
    same_names(&pair, "moved");
    assert!(pair.alt.join("d/empty").is_dir() && !pair.alt.join("e/empty").exists());
    assert!(pair.alt.join("d/s/hollow").is_dir() && pair.alt.join("d/deep/er").is_dir());
    pair.both(&["switch", "main"]);
    pair.assert_same_state();
    same_names(&pair, "back");
    // taken back, they are where they were
    ok(alt(&pair.alt, &["undo"]));
    assert!(pair.alt.join("d/empty").is_dir() && !pair.alt.join("e/empty").exists());
    assert!(pair.alt.join("e/x").is_file() && !pair.alt.join("d/x").exists());
}

#[test]
fn a_directory_that_moves_whole_leaves_the_files_no_tree_has() {
    for left in ["d/mine", "d/s/a.ign"] {
        let pair = moved();
        pair.write(left, "mine\n");
        pair.both(&["switch", "moved"]);
        pair.assert_same_state();
        same_names(&pair, left);
        assert!(pair.alt.join(left).is_file(), "{left}");
        pair.both(&["switch", "main"]);
        pair.assert_same_state();
        same_names(&pair, left);
    }
}

#[test]
fn a_directory_moved_by_name_takes_all_it_holds_and_an_undo_brings_it_back() {
    let pair = moved();
    make(&pair, &["d/empty", "d/s/hollow", "d/deep/er", "into"]);
    pair.write("d/mine", "mine\n");
    pair.write("d/s/a.ign", "ignored\n");
    let before = spelled(&pair.alt);
    pair.both(&["mv", "d", "f"]);
    pair.assert_same_state();
    same_names(&pair, "moved");
    let there = spelled(&pair.alt);
    for dir in ["f/empty", "f/s/hollow", "f/deep/er"] {
        assert!(pair.alt.join(dir).is_dir(), "{dir}");
    }
    assert!(!pair.alt.join("d").exists());
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(spelled(&pair.alt), before);
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(spelled(&pair.alt), there);
    // into a directory that is there
    pair.both(&["mv", "f", "into"]);
    pair.assert_same_state();
    same_names(&pair, "into");
    assert!(pair.alt.join("into/f/deep/er").is_dir());
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(spelled(&pair.alt), there);
}

/// A directory that came to hold something since is no longer the one
/// that was moved: the undo says so and changes nothing.
#[test]
fn an_undo_of_a_move_is_refused_where_an_empty_directory_holds_something_by_now() {
    let pair = moved();
    make(&pair, &["d/empty"]);
    ok(alt(&pair.alt, &["mv", "d", "f"]));
    std::fs::write(pair.alt.join("f/empty/new"), "n\n").unwrap();
    let there = spelled(&pair.alt);
    let undone = alt(&pair.alt, &["undo"]);
    assert!(!undone.status.success());
    let said = String::from_utf8_lossy(&undone.stderr).into_owned();
    assert!(said.contains("'f/empty' changed"), "{said}");
    assert_eq!(spelled(&pair.alt), there);
}
