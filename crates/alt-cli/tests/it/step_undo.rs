//! `alt undo` after a command that began an operation, went on with it or
//! ended it: the workspace is as it was before the command, the file that
//! says the operation is under way included. Run directly and through the
//! daemon.

use std::collections::BTreeMap;
use std::path::Path;

use crate::byte_names::entries;
use crate::cwd_paths::Trees;
use crate::scan_daemon::output;
use crate::stopped_trees::{commit, diverged, every, working_tree};

pub(crate) const STATE_FILES: [&str; 4] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_STATE",
    "REBASE_STATE",
];

/// Everything of a workspace an undo puts back.
#[derive(Debug, PartialEq)]
pub(crate) struct Seen {
    index: String,
    files: BTreeMap<String, (Vec<u8>, bool)>,
    head: String,
    status: String,
    state: Vec<Option<String>>,
}

pub(crate) fn seen(trees: &Trees, tree: &str) -> Seen {
    let at = trees.at(tree);
    let said = |args: &[&str]| {
        let done = output(&at, args, false);
        assert!(done.status.success(), "{tree}: {args:?}");
        String::from_utf8(done.stdout).unwrap()
    };
    let state = |name: &&str| std::fs::read_to_string(at.join(".alt").join(name)).ok();
    Seen {
        index: String::from_utf8_lossy(&entries(trees, &at.join(".alt/index"))).into_owned(),
        files: working_tree(trees, tree),
        head: said(&["rev-parse", "HEAD"]),
        status: said(&["status", "--json"]),
        state: STATE_FILES.iter().map(state).collect(),
    }
}

/// Runs `args` in the tree, through the daemon in the one it serves.
pub(crate) fn code(at: &Path, args: &[&str]) -> Option<i32> {
    output(at, args, at.ends_with("served")).status.code()
}

/// Runs `command`, takes it back, and finds the workspace as it was.
fn taken_back(trees: &Trees, tree: &str, command: &[&str], exit: i32) {
    let at = trees.at(tree);
    let before = seen(trees, tree);
    assert_eq!(code(&at, command), Some(exit), "{tree}: {command:?}");
    assert_ne!(seen(trees, tree), before, "{tree}: {command:?} did nothing");
    let undone = output(&at, &["undo"], at.ends_with("served"));
    let said = String::from_utf8_lossy(&undone.stderr).into_owned();
    assert!(
        undone.status.success(),
        "{tree}: undo of {command:?}: {said}"
    );
    assert_eq!(seen(trees, tree), before, "{tree}: after {command:?}");
}

fn resolve(at: &Path, files: &[&str]) {
    for file in files {
        std::fs::write(at.join(file), "resolved\n").unwrap();
    }
    let mut add = vec!["add"];
    add.extend(files);
    assert_eq!(code(at, &add), Some(0));
}

fn parents(at: &Path) -> usize {
    let commit = output(at, &["cat-file", "-p", "HEAD"], false).stdout;
    let commit = String::from_utf8(commit).unwrap();
    commit.lines().filter(|l| l.starts_with("parent ")).count()
}

#[test]
fn a_merge_that_stopped_given_up_or_committed_comes_back_whole() {
    let trees = diverged();
    for tree in ["direct", "served"] {
        let at = trees.at(tree);
        let clean = seen(&trees, tree);
        // the merge itself, which no record but the step tells of
        taken_back(&trees, tree, &["merge", "side"], 1);
        assert_eq!(code(&at, &["merge", "side"]), Some(1));
        taken_back(&trees, tree, &["merge", "--abort"], 0);
        // the undo taken back in turn gives the merge up again
        assert_eq!(code(&at, &["undo"]), Some(0));
        assert_eq!(seen(&trees, tree), clean, "{tree}: the abort done again");
        assert_eq!(code(&at, &["undo"]), Some(0));
        resolve(&at, &["f.txt", "both.txt"]);
        taken_back(&trees, tree, &["commit", "-m", "merged"], 0);
        // the merge is still one: the commit that concludes it has both parents
        assert_eq!(code(&at, &["commit", "-m", "merged"]), Some(0));
        assert_eq!(parents(&at), 2, "{tree}");
    }
}

#[test]
fn a_cherry_pick_given_up_gone_on_with_or_committed_comes_back_whole() {
    let trees = diverged();
    for tree in ["direct", "served"] {
        let at = trees.at(tree);
        taken_back(&trees, tree, &["cherry-pick", "side"], 1);
        assert_eq!(code(&at, &["cherry-pick", "side"]), Some(1));
        taken_back(&trees, tree, &["cherry-pick", "--abort"], 0);
        resolve(&at, &["f.txt", "both.txt"]);
        taken_back(&trees, tree, &["cherry-pick", "--continue"], 0);
        taken_back(&trees, tree, &["commit", "-m", "picked by hand"], 0);
        assert_eq!(code(&at, &["cherry-pick", "--continue"]), Some(0));
    }
}

#[test]
fn a_revert_given_up_or_gone_on_with_comes_back_whole() {
    let trees = Trees::new();
    trees.write("f.txt", "1\n2\n3\n");
    commit(&trees, "f");
    trees.write("f.txt", "1\nb\n3\n");
    trees.write("g.txt", "g\n");
    commit(&trees, "to b, and g");
    trees.write("f.txt", "1\nc\n3\n");
    commit(&trees, "to c");
    trees.write("h.txt", "h\n");
    commit(&trees, "h");
    trees.write("i.txt", "i\n");
    commit(&trees, "i");
    for tree in ["direct", "served"] {
        let at = trees.at(tree);
        // the first is reverted and committed, the second stops, the third waits
        let revert = ["revert", "HEAD~1", "HEAD~3", "HEAD"];
        taken_back(&trees, tree, &revert, 1);
        assert_eq!(code(&at, &revert), Some(1));
        // the abort moves the branch back as well
        taken_back(&trees, tree, &["revert", "--abort"], 0);
        resolve(&at, &["f.txt"]);
        // commits the one that stopped and the one that waited
        taken_back(&trees, tree, &["revert", "--continue"], 0);
        taken_back(&trees, tree, &["commit", "-m", "reverted by hand"], 0);
        assert_eq!(code(&at, &["revert", "--continue"]), Some(0));
        assert!(!at.join("i.txt").exists() && !at.join("h.txt").exists());
    }
}

#[test]
fn a_rebase_given_up_skipped_or_gone_on_with_comes_back_whole() {
    let trees = diverged();
    assert_eq!(every(&trees, &["switch", "side"]), [Some(0); 3]);
    trees.write("f.txt", "1\nside again\n3\n");
    commit(&trees, "side again");
    trees.write("s.txt", "s\n");
    commit(&trees, "side adds s");
    for tree in ["direct", "served"] {
        let at = trees.at(tree);
        taken_back(&trees, tree, &["rebase", "main"], 1);
        assert_eq!(code(&at, &["rebase", "main"]), Some(1));
        taken_back(&trees, tree, &["rebase", "--abort"], 0);
        resolve(&at, &["f.txt", "both.txt"]);
        // commits the step and stops at the next
        taken_back(&trees, tree, &["rebase", "--continue"], 1);
        assert_eq!(code(&at, &["rebase", "--continue"]), Some(1));
        // drops the step, replays the last and moves the branch
        taken_back(&trees, tree, &["rebase", "--skip"], 0);
        std::fs::write(at.join("f.txt"), "resolved again\n").unwrap();
        assert_eq!(code(&at, &["add", "f.txt"]), Some(0));
        taken_back(&trees, tree, &["commit", "-m", "the step by hand"], 0);
        taken_back(&trees, tree, &["rebase", "--continue"], 0);
        assert_eq!(code(&at, &["rebase", "--continue"]), Some(0));
        assert_eq!(std::fs::read(at.join("s.txt")).unwrap(), b"s\n");
    }
}

#[test]
fn the_record_of_a_step_is_an_operation_that_is_done() {
    let trees = diverged();
    let at = trees.at("direct");
    assert_eq!(code(&at, &["merge", "side"]), Some(1));
    let log = output(&at, &["op-log", "-n", "1", "--json"], false).stdout;
    let log = String::from_utf8(log).unwrap();
    assert!(log.contains(r#""verb":"step""#), "{log}");
    let id = log.split(r#""id":""#).nth(1).unwrap();
    let id = &id[..id.find('"').unwrap()];
    let status = output(&at, &["operation", "status", id], false);
    let status = String::from_utf8(status.stdout).unwrap();
    assert_eq!(status, format!("{id} completed\n"));
    let pending = output(&at, &["operation", "pending"], false);
    assert_eq!(String::from_utf8_lossy(&pending.stdout), "");
}
