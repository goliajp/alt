//! `alt reset`, to a commit in each of its modes and of paths, beside git.

use crate::like_git::{Pair, alt, alt_daemon, fails, files, git, heads, index, index_at, ok};

/// Three commits on `main`; the second changes `a` and adds `d/n`, the
/// third changes `a` again.
fn history() -> Pair {
    let pair = Pair::new();
    pair.write("a", "a\n");
    pair.write("b", "b\n");
    pair.write("d/x", "x\n");
    pair.commit_all("base");
    pair.write("a", "a2\n");
    pair.write("d/n", "n\n");
    pair.commit_all("second");
    pair.write("a", "a3\n");
    pair.commit_all("third commit");
    pair
}

/// A change that is staged, one that is not, and a file not tracked.
fn dirty(pair: &Pair) {
    pair.write("a", "a4\n");
    pair.write("b", "b2\n");
    pair.both(&["add", "b"]);
    pair.write("untracked", "u\n");
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
    pair
}

#[test]
fn a_mixed_reset_unstages_and_lists_what_is_left() {
    let pair = history();
    dirty(&pair);
    let said = pair.same(&["reset"]);
    assert_eq!(said, "Unstaged changes after reset:\nM\ta\nM\tb\n");
    pair.assert_same_state();
    assert_eq!(pair.same(&["reset", "-q"]), "");
    assert_eq!(pair.same(&["reset", "--mixed", "HEAD~1"]), said);
    pair.assert_same_state();
    assert!(pair.alt.join("untracked").exists());
}

#[test]
fn a_soft_reset_moves_the_branch_alone() {
    let pair = history();
    dirty(&pair);
    assert_eq!(pair.same(&["reset", "--soft", "HEAD~2"]), "");
    pair.assert_same_state();
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "a4\n");
}

#[test]
fn a_hard_reset_brings_the_files_along() {
    let pair = history();
    dirty(&pair);
    let said = pair.same(&["reset", "--hard", "HEAD~1"]);
    assert!(
        said.starts_with("HEAD is now at ") && said.ends_with(" second\n"),
        "{said}"
    );
    pair.assert_same_state();
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "a2\n");
    assert!(pair.alt.join("untracked").exists());
}

#[test]
fn a_hard_reset_writes_over_an_untracked_file_in_its_way() {
    let pair = history();
    pair.both(&["branch", "tip"]);
    pair.same(&["reset", "--hard", "HEAD~2"]);
    assert!(!pair.alt.join("d/n").exists());
    pair.write("d/n", "mine\n");
    pair.same(&["reset", "--hard", "tip"]);
    pair.assert_same_state();
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("d/n")).unwrap(),
        "n\n"
    );
}

#[test]
fn what_cannot_be_reset_is_refused_as_git_refuses_it() {
    let pair = history();
    dirty(&pair);
    let said = pair.same_failure(&["reset", "--hard", "nosuch"], 128);
    assert!(
        said.starts_with("fatal: ambiguous argument 'nosuch'"),
        "{said}"
    );
    let said = pair.same_failure(&["reset", "--hard", "HEAD", "--", "a"], 128);
    assert_eq!(said, "fatal: Cannot do hard reset with paths.\n");
    pair.same_failure(&["reset", "--soft", "--", "a"], 128);
    pair.assert_same_state();
    fails(alt(&pair.alt, &["reset", "--soft", "--hard"]), 2);
}

#[test]
fn paths_have_their_index_entries_reset_and_their_files_left() {
    let pair = history();
    dirty(&pair);
    pair.write("d/x", "x2\n");
    pair.both(&["add", "a", "d/x"]);
    assert_eq!(
        pair.same(&["reset", "--", "a"]),
        "Unstaged changes after reset:\nM\ta\n"
    );
    pair.assert_same_state();
    pair.same(&["reset", "HEAD~1", "--", "d", "a"]);
    pair.assert_same_state();
    pair.same(&["reset", "b"]);
    pair.same(&["reset", "HEAD", "d"]);
    pair.assert_same_state();
    // a path that names nothing is no error
    pair.same(&["reset", "-q", "--", "nope"]);
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("d/x")).unwrap(),
        "x2\n"
    );

    pair.both(&["add", "d/x"]);
    ok(alt(&pair.alt.join("d"), &["reset", "-q", "--", "x"]));
    ok(git(&pair.git.join("d"), &["reset", "-q", "--", "x"]));
    pair.assert_same_state();
}

#[test]
fn a_reset_gives_up_the_merge_that_stopped() {
    let pair = conflicted();
    let said = pair.same_failure(&["reset", "--soft", "HEAD"], 128);
    assert_eq!(
        said,
        "fatal: Cannot do a soft reset in the middle of a merge.\n"
    );
    assert_eq!(
        pair.same(&["reset"]),
        "Unstaged changes after reset:\nM\ta\n"
    );
    pair.assert_same_state();
    let none = pair.same_failure(&["merge", "--abort"], 128);
    assert!(none.contains("There is no merge to abort"), "{none}");

    let pair = conflicted();
    pair.same(&["reset", "--hard"]);
    pair.assert_same_state();
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("a")).unwrap(),
        "amain\n"
    );
    assert!(ok(alt(&pair.alt, &["status"])).contains("working tree clean"));
    assert!(!pair.alt.join(".alt/MERGE_HEAD").exists());
}

#[test]
fn a_reset_of_paths_leaves_the_merge_under_way() {
    let pair = conflicted();
    pair.same(&["reset", "-q", "--", "a"]);
    pair.assert_same_state();
    assert!(pair.alt.join(".alt/MERGE_HEAD").exists());
}

#[test]
fn a_reset_in_a_repository_without_a_commit() {
    let pair = Pair::new();
    pair.write("a", "a\n");
    pair.both(&["add", "a"]);
    assert_eq!(pair.same(&["reset"]), "");
    assert_eq!(index(&pair.alt), "");
    pair.both(&["add", "a"]);
    pair.same(&["reset", "--hard"]);
    assert!(!pair.alt.join("a").exists() && !pair.git.join("a").exists());
    pair.same_failure(&["reset", "--hard", "HEAD"], 128);
}

#[test]
fn a_reset_is_undone_with_the_merge_it_gave_up() {
    let pair = history();
    dirty(&pair);
    let before = (files(&pair.alt), index(&pair.alt), heads(&pair.alt));
    ok(alt(&pair.alt, &["reset", "--hard", "HEAD~2"]));
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(
        (files(&pair.alt), index(&pair.alt), heads(&pair.alt)),
        before
    );
    ok(alt(&pair.alt, &["reset", "--soft", "HEAD~1"]));
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(
        (files(&pair.alt), index(&pair.alt), heads(&pair.alt)),
        before
    );

    let pair = conflicted();
    let before = (files(&pair.alt), index(&pair.alt), heads(&pair.alt));
    ok(alt(&pair.alt, &["reset", "--hard"]));
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(
        (files(&pair.alt), index(&pair.alt), heads(&pair.alt)),
        before
    );
    assert!(pair.alt.join(".alt/MERGE_HEAD").exists());
    ok(alt(&pair.alt, &["merge", "--abort"]));
}

#[test]
fn the_daemon_resets_as_the_command_itself_does() {
    let (direct, served) = (history(), history());
    dirty(&direct);
    dirty(&served);
    for args in [
        &["reset", "HEAD~1"][..],
        &["reset", "--hard", "HEAD~1"],
        &["reset", "--", "a"],
    ] {
        assert_eq!(
            ok(alt_daemon(&served.alt, args)),
            ok(alt(&direct.alt, args))
        );
        assert_eq!(files(&served.alt), files(&direct.alt));
        assert_eq!(index(&served.alt), index(&direct.alt));
        assert_eq!(heads(&served.alt), heads(&direct.alt));
    }
}

#[test]
fn a_parallel_workspace_resets_its_own_branch_and_files() {
    let pair = history();
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap()],
    ));
    std::fs::write(tree.join("b"), "ws\n").unwrap();
    ok(alt(&tree, &["add", "b"]));
    let said = ok(alt(
        &pair.alt,
        &["--workspace", "ws", "reset", "--hard", "HEAD~1"],
    ));
    assert!(said.ends_with(" second\n"), "{said}");
    assert_eq!(std::fs::read_to_string(tree.join("a")).unwrap(), "a2\n");
    assert_eq!(std::fs::read_to_string(tree.join("b")).unwrap(), "b\n");
    ok(git(&pair.git, &["reset", "-q", "--hard", "HEAD~1"]));
    let staged = index_at(&pair.alt, &pair.alt.join(".alt/workspaces/ws/index"));
    assert_eq!(staged, ok(git(&pair.git, &["ls-files", "-s"])));
    // the default workspace and its branch stay
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "a3\n");
    assert!(ok(alt(&pair.alt, &["log", "-n", "1"])).contains("third commit"));
}

#[test]
fn a_protected_flow_branch_is_not_moved_by_a_reset() {
    let pair = history();
    ok(alt(&pair.alt, &["flow", "init"]));
    let at = heads(&pair.alt);
    let said = fails(alt(&pair.alt, &["reset", "--hard", "HEAD~1"]), 1);
    assert!(said.contains("protected branch 'develop'"), "{said}");
    assert_eq!(heads(&pair.alt), at);
    // what moves no branch is not refused
    ok(alt(&pair.alt, &["reset", "--hard"]));
    ok(alt(&pair.alt, &["reset", "--", "a"]));
}
