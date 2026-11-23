//! `alt restore` and the path form of `alt checkout`, each beside git.

use crate::like_git::{Pair, alt, alt_daemon, fails, files, git, index, index_at, ok};

/// Two commits on `main`: the second changes `a` and adds `d/n`.
fn history() -> Pair {
    let pair = Pair::new();
    pair.write("a", "a\n");
    pair.write("b", "b\n");
    pair.write("d/x", "x\n");
    pair.write("d/y", "y\n");
    pair.commit_all("base");
    pair.write("a", "a2\n");
    pair.write("d/n", "n\n");
    pair.commit_all("second");
    pair
}

/// [`history`] with a merge of `side` stopped in conflict at `a`.
fn conflicted() -> Pair {
    let pair = history();
    pair.both(&["switch", "-c", "side"]);
    pair.write("a", "aside\n");
    pair.commit_all("side");
    pair.both(&["switch", "main"]);
    pair.write("a", "amain\n");
    pair.commit_all("main");
    let (merged, git_merged) = pair.run(&["merge", "side"]);
    assert_eq!(
        (merged.status.code(), git_merged.status.code()),
        (Some(1), Some(1))
    );
    pair.assert_same_state();
    pair
}

#[test]
fn files_are_put_back_as_the_index_has_them() {
    let pair = history();
    pair.write("a", "a3\n");
    pair.remove("b");
    pair.write("d/x", "x2\n");
    pair.both(&["add", "d/x"]);
    pair.write("d/x", "x3\n");
    assert_eq!(pair.same(&["restore", "a", "b", "d/x"]), "");
    pair.assert_same_state();
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("d/x")).unwrap(),
        "x2\n"
    );
}

#[test]
fn a_path_that_names_nothing_and_no_path_at_all_are_refused() {
    let pair = history();
    let said = pair.same_failure(&["restore", "nope"], 1);
    assert_eq!(
        said,
        "error: pathspec 'nope' did not match any file(s) known to git\n"
    );
    pair.same_failure(&["restore"], 128);
    // git words this one differently from one version to the next
    let said = fails(alt(&pair.alt, &["restore", "--source", "nosuch", "a"]), 128);
    assert!(
        said.starts_with("fatal: could not resolve 'nosuch'\n"),
        "{said}"
    );
    fails(git(&pair.git, &["restore", "--source", "nosuch", "a"]), 128);
    pair.assert_same_state();
}

#[test]
fn staged_puts_the_index_back_and_leaves_the_files() {
    let pair = history();
    pair.write("a", "a3\n");
    pair.write("new", "new\n");
    pair.both(&["add", "a", "new"]);
    pair.write("a", "a4\n");
    assert_eq!(pair.same(&["restore", "--staged", "a", "new"]), "");
    pair.assert_same_state();
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "a4\n");
    assert!(pair.alt.join("new").exists());

    pair.both(&["add", "a"]);
    pair.same(&["restore", "--staged", "--worktree", "a"]);
    pair.assert_same_state();
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "a2\n");
}

#[test]
fn a_source_is_restored_from_with_what_it_lacks_removed() {
    let pair = history();
    pair.same(&["restore", "--source", "HEAD~1", "a", "d"]);
    pair.assert_same_state();
    assert!(!pair.alt.join("d/n").exists());
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "a\n");

    pair.same(&["restore", "--source", "HEAD~1", "--staged", "d"]);
    pair.assert_same_state();
    pair.same(&["restore", "-s", "HEAD", "-S", "-W", "."]);
    pair.assert_same_state();
    assert!(pair.alt.join("d/n").exists());
}

#[test]
fn a_path_is_named_from_the_directory_the_command_runs_in() {
    let pair = history();
    pair.write("d/x", "x2\n");
    pair.write("a", "a3\n");
    ok(alt(&pair.alt.join("d"), &["restore", "x", "../a"]));
    ok(git(&pair.git.join("d"), &["restore", "x", "../a"]));
    pair.assert_same_state();
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("d/x")).unwrap(),
        "x\n"
    );
}

#[test]
fn a_path_in_conflict_takes_the_side_asked_for() {
    let pair = conflicted();
    let said = pair.same_failure(&["restore", "a"], 1);
    assert_eq!(said, "error: path 'a' is unmerged\n");
    pair.same(&["restore", "--ours", "a"]);
    pair.assert_same_state();
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("a")).unwrap(),
        "amain\n"
    );
    pair.same(&["restore", "--theirs", "a"]);
    pair.assert_same_state();
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("a")).unwrap(),
        "aside\n"
    );
    pair.same_failure(&["restore", "--ours", "--staged", "a"], 128);

    // the index is put back whole from HEAD
    pair.same(&["restore", "--staged", "a"]);
    pair.assert_same_state();
}

#[test]
fn checkout_takes_a_side_of_a_conflict() {
    let pair = conflicted();
    pair.same_failure(&["checkout", "a"], 1);
    ok(alt(&pair.alt, &["checkout", "--ours", "a"]));
    ok(git(&pair.git, &["checkout", "--ours", "a"]));
    pair.assert_same_state();
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("a")).unwrap(),
        "amain\n"
    );
    ok(alt(&pair.alt, &["checkout", "--theirs", "--", "a"]));
    ok(git(&pair.git, &["checkout", "--theirs", "--", "a"]));
    pair.assert_same_state();
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("a")).unwrap(),
        "aside\n"
    );
    pair.same_failure(&["checkout", "--ours"], 128);
}

#[test]
fn checkout_restores_paths_and_switches_branches() {
    let pair = history();
    pair.write("a", "a3\n");
    ok(alt(&pair.alt, &["checkout", "a"]));
    ok(git(&pair.git, &["checkout", "a"]));
    pair.assert_same_state();

    // from a revision, index and files; what it lacks stays
    ok(alt(&pair.alt, &["checkout", "HEAD~1", "a", "d"]));
    ok(git(&pair.git, &["checkout", "HEAD~1", "a", "d"]));
    pair.assert_same_state();
    assert!(pair.alt.join("d/n").exists());
    pair.same(&["restore", "-s", "HEAD", "-S", "-W", "."]);

    ok(alt(&pair.alt, &["checkout", "-b", "topic"]));
    ok(git(&pair.git, &["checkout", "-q", "-b", "topic"]));
    ok(alt(&pair.alt, &["checkout", "main"]));
    ok(git(&pair.git, &["checkout", "-q", "main"]));
    pair.assert_same_state();
}

#[test]
fn a_restore_is_undone() {
    let pair = history();
    pair.write("a", "a3\n");
    pair.write("b", "b2\n");
    ok(alt(&pair.alt, &["add", "b"]));
    let (before, staged) = (files(&pair.alt), index(&pair.alt));
    ok(alt(
        &pair.alt,
        &["restore", "--staged", "--worktree", "a", "b"],
    ));
    assert_ne!(files(&pair.alt), before);
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!((files(&pair.alt), index(&pair.alt)), (before, staged));
}

#[test]
fn the_daemon_restores_as_the_command_itself_does() {
    let (direct, served) = (history(), history());
    for pair in [&direct, &served] {
        pair.write("a", "a3\n");
        pair.write("b", "b2\n");
        ok(alt(&pair.alt, &["add", "b"]));
    }
    let args = [
        "restore",
        "--json",
        "--source",
        "HEAD~1",
        "--staged",
        "--worktree",
        "a",
        "b",
        "d",
    ];
    let said = ok(alt(&direct.alt, &args));
    let served_said = ok(alt_daemon(&served.alt, &args));
    let paths = |said: &str| {
        said.split("\"paths\":")
            .nth(1)
            .map(|rest| rest.split(']').next().unwrap().to_owned())
    };
    assert_eq!(paths(&said).as_deref(), Some("[\"a\",\"b\",\"d/n\""));
    assert_eq!(paths(&served_said), paths(&said));
    assert_eq!(files(&served.alt), files(&direct.alt));
    assert_eq!(index(&served.alt), index(&direct.alt));
}

#[test]
fn a_parallel_workspace_restores_its_own_files() {
    let pair = history();
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap()],
    ));
    std::fs::write(tree.join("a"), "ws\n").unwrap();
    std::fs::write(tree.join("b"), "ws\n").unwrap();
    ok(alt(&pair.alt, &["--workspace", "ws", "add", "b"]));
    std::fs::write(pair.alt.join("a"), "default\n").unwrap();
    ok(alt(&tree, &["restore", "a"]));
    ok(alt(
        &pair.alt,
        &["--workspace", "ws", "restore", "--staged", "b"],
    ));
    assert_eq!(std::fs::read_to_string(tree.join("a")).unwrap(), "a2\n");
    assert_eq!(std::fs::read_to_string(tree.join("b")).unwrap(), "ws\n");
    let staged = index_at(&pair.alt, &pair.alt.join(".alt/workspaces/ws/index"));
    assert_eq!(staged, index(&pair.alt));
    // the default workspace keeps its change
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("a")).unwrap(),
        "default\n"
    );
    fails(alt(&pair.alt, &["--workspace", "ws", "restore", "nope"]), 1);
}
