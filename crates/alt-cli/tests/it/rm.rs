//! `alt rm` beside git: what it removes, what it says, and when it
//! refuses to lose a change.

use crate::like_git::{Pair, alt, alt_daemon, fails, files, git, index, index_at, ok};

fn tree() -> Pair {
    let pair = Pair::new();
    for name in ["a", "b", "c", "f", "d/x", "d/s/y"] {
        pair.write(name, &format!("{name}\n"));
    }
    pair.commit_all("base");
    pair
}

#[test]
fn a_file_goes_from_the_index_and_the_tree() {
    let pair = tree();
    assert_eq!(pair.same(&["rm", "a"]), "rm 'a'\n");
    pair.assert_same_state();
    assert!(!pair.alt.join("a").exists());
    assert_eq!(pair.same(&["rm", "-q", "f"]), "");
    // a file that is already gone has nothing to lose
    pair.remove("b");
    assert_eq!(pair.same(&["rm", "b"]), "rm 'b'\n");
    pair.assert_same_state();
}

#[test]
fn a_directory_goes_only_with_r() {
    let pair = tree();
    let said = pair.same_failure(&["rm", "d"], 128);
    assert_eq!(said, "fatal: not removing 'd' recursively without -r\n");
    assert_eq!(pair.same(&["rm", "-r", "d"]), "rm 'd/s/y'\nrm 'd/x'\n");
    pair.assert_same_state();
    assert!(!pair.alt.join("d").exists());
}

#[test]
fn a_path_is_named_from_the_directory_the_command_runs_in() {
    let pair = tree();
    let said = ok(alt(&pair.alt.join("d"), &["rm", "x", "../a"]));
    assert_eq!(said, ok(git(&pair.git.join("d"), &["rm", "x", "../a"])));
    assert_eq!(said, "rm 'a'\nrm 'd/x'\n");
    pair.assert_same_state();
}

#[test]
fn what_names_nothing_is_refused_unless_told_to_pass_it_over() {
    let pair = tree();
    let said = pair.same_failure(&["rm", "nope"], 128);
    assert_eq!(said, "fatal: pathspec 'nope' did not match any files\n");
    pair.write("u", "u\n");
    pair.same_failure(&["rm", "u"], 128);
    pair.same_failure(&["rm", "a", "nope"], 128);
    assert!(pair.alt.join("a").exists());
    pair.same_failure(&["rm"], 128);
    assert_eq!(
        pair.same(&["rm", "--ignore-unmatch", "nope", "a"]),
        "rm 'a'\n"
    );
    pair.assert_same_state();
}

#[test]
fn a_change_that_would_be_lost_refuses_the_removal() {
    let pair = tree();
    pair.write("a", "a2\n");
    let said = pair.same_failure(&["rm", "a"], 1);
    assert!(
        said.contains("the following file has local modifications:\n    a\n"),
        "{said}"
    );
    pair.write("b", "b2\n");
    pair.both(&["add", "b"]);
    let said = pair.same_failure(&["rm", "b"], 1);
    assert!(said.contains("has changes staged in the index:"), "{said}");
    pair.write("c", "c2\n");
    pair.both(&["add", "c"]);
    pair.write("c", "c3\n");
    let said = pair.same_failure(&["rm", "--cached", "c"], 1);
    assert!(
        said.contains("staged content different from both"),
        "{said}"
    );
    pair.write("n", "n\n");
    pair.both(&["add", "n"]);
    // each reason with its files, several at once
    pair.same_failure(&["rm", "a", "b", "c", "n", "f"], 1);
    pair.assert_same_state();

    assert_eq!(
        pair.same(&["rm", "--cached", "a", "b", "n"]),
        "rm 'a'\nrm 'b'\nrm 'n'\n"
    );
    pair.assert_same_state();
    assert!(pair.alt.join("a").exists() && pair.alt.join("n").exists());
    assert_eq!(pair.same(&["rm", "-f", "c"]), "rm 'c'\n");
    pair.assert_same_state();
}

#[test]
fn a_dry_run_says_and_removes_nothing() {
    let pair = tree();
    assert_eq!(
        pair.same(&["rm", "-n", "-r", "d", "a"]),
        "rm 'a'\nrm 'd/s/y'\nrm 'd/x'\n"
    );
    pair.assert_same_state();
    assert!(pair.alt.join("a").exists());
}

#[test]
fn a_removal_is_undone() {
    let pair = tree();
    let before = (files(&pair.alt), index(&pair.alt));
    ok(alt(&pair.alt, &["rm", "-r", "d", "a"]));
    assert_ne!(files(&pair.alt), before.0);
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!((files(&pair.alt), index(&pair.alt)), before);
}

#[test]
fn the_daemon_removes_as_the_command_itself_does() {
    let (direct, served) = (tree(), tree());
    let args = ["rm", "-r", "d", "a"];
    assert_eq!(
        ok(alt_daemon(&served.alt, &args)),
        ok(alt(&direct.alt, &args))
    );
    assert_eq!(files(&served.alt), files(&direct.alt));
    assert_eq!(index(&served.alt), index(&direct.alt));
    let json = ok(alt_daemon(&served.alt, &["rm", "--json", "b"]));
    assert!(json.contains("\"paths\":[\"b\"]"), "{json}");
}

#[test]
fn a_parallel_workspace_removes_its_own_files() {
    let pair = tree();
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap()],
    ));
    assert_eq!(ok(alt(&tree, &["rm", "-r", "d"])), "rm 'd/s/y'\nrm 'd/x'\n");
    ok(alt(
        &pair.alt,
        &["--workspace", "ws", "rm", "--cached", "a"],
    ));
    ok(git(&pair.git, &["rm", "-q", "-r", "d"]));
    ok(git(&pair.git, &["rm", "-q", "--cached", "a"]));
    let staged = index_at(&pair.alt, &pair.alt.join(".alt/workspaces/ws/index"));
    assert_eq!(staged, ok(git(&pair.git, &["ls-files", "-s"])));
    assert_eq!(files(&tree), files(&pair.git));
    assert!(pair.alt.join("d/x").exists());
    fails(alt(&pair.alt, &["--workspace", "ws", "rm", "nope"]), 128);
}
