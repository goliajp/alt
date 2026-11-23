//! `alt switch` with changes that are not committed: carried over where
//! the two commits agree on the path, refused where they would be
//! written over, each beside what git does.

use crate::like_git::{Pair, alt, alt_daemon, fails, files, git, heads, index, index_at, ok};

/// `main` with four files, and `other` one commit ahead that changes
/// `a`, removes `d` and adds `n`; both are left on `main`.
fn branches() -> Pair {
    let pair = Pair::new();
    for name in ["a", "b", "c", "d"] {
        pair.write(name, &format!("{name}\n"));
    }
    pair.commit_all("base");
    pair.both(&["switch", "-c", "other"]);
    pair.write("a", "a2\n");
    pair.write("n", "new\n");
    pair.remove("d");
    pair.commit_all("other");
    pair.both(&["switch", "main"]);
    pair
}

/// Changes on paths the two commits agree on: one not staged, one
/// staged, and a new file staged.
fn change_apart(pair: &Pair) {
    pair.write("b", "b2\n");
    pair.write("c", "c2\n");
    pair.write("e", "e\n");
    pair.both(&["add", "c", "e"]);
}

#[test]
fn changes_on_paths_the_commits_agree_on_are_carried() {
    let pair = branches();
    change_apart(&pair);
    let said = ok(alt(&pair.alt, &["switch", "other"]));
    let git_said = ok(git(&pair.git, &["switch", "other"]));
    assert_eq!(git_said, "M\tb\nM\tc\nA\te\n");
    assert_eq!(said, format!("{git_said}Switched to branch 'other'\n"));
    pair.assert_same_state();
    pair.same(&["diff", "--cached"]);

    // and back again
    pair.both(&["switch", "main"]);
    pair.assert_same_state();
}

#[test]
fn a_change_the_switch_would_write_over_refuses_it() {
    let pair = branches();
    pair.write("a", "a3\n");
    pair.write("d", "dd\n");
    pair.write("b", "b2\n");
    let (before, staged) = (files(&pair.alt), index(&pair.alt));
    let said = fails(alt(&pair.alt, &["switch", "other"]), 1);
    let git_said = fails(git(&pair.git, &["switch", "other"]), 1);
    assert!(said.starts_with(&git_said), "alt: {said}\ngit: {git_said}");
    assert!(git_said.contains("\ta\n\td\n"), "{git_said}");
    assert_eq!((files(&pair.alt), index(&pair.alt)), (before, staged));
    pair.assert_same_state();
}

#[test]
fn a_staged_change_the_switch_would_write_over_refuses_it() {
    let pair = branches();
    pair.write("a", "a4\n");
    pair.both(&["add", "a"]);
    let said = fails(alt(&pair.alt, &["switch", "other"]), 1);
    let git_said = fails(git(&pair.git, &["switch", "other"]), 1);
    assert!(said.starts_with(&git_said), "alt: {said}\ngit: {git_said}");
    pair.assert_same_state();
}

#[test]
fn a_path_staged_as_the_target_has_it_keeps_its_file() {
    let pair = branches();
    pair.write("a", "a2\n");
    pair.both(&["add", "a"]);
    pair.write("a", "a5\n");
    let said = ok(alt(&pair.alt, &["switch", "other"]));
    let git_said = ok(git(&pair.git, &["switch", "other"]));
    assert_eq!(said, format!("{git_said}Switched to branch 'other'\n"));
    assert_eq!(git_said, "M\ta\n");
    pair.assert_same_state();
}

#[test]
fn a_file_deleted_from_the_tree_does_not_stand_in_the_way() {
    let pair = branches();
    pair.remove("a");
    pair.remove("d");
    pair.both(&["switch", "other"]);
    pair.assert_same_state();
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "a2\n");
}

#[test]
fn an_untracked_file_the_switch_would_write_over_refuses_it() {
    let pair = branches();
    pair.write("n", "mine\n");
    let said = fails(alt(&pair.alt, &["switch", "other"]), 1);
    let git_said = fails(git(&pair.git, &["switch", "other"]), 1);
    assert!(said.starts_with(&git_said), "alt: {said}\ngit: {git_said}");
    pair.assert_same_state();
}

#[test]
fn a_carrying_switch_is_undone_with_the_changes_in_place() {
    let pair = branches();
    change_apart(&pair);
    let (before, staged, at) = (files(&pair.alt), index(&pair.alt), heads(&pair.alt));
    ok(alt(&pair.alt, &["switch", "other"]));
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(
        (files(&pair.alt), index(&pair.alt), heads(&pair.alt)),
        (before, staged, at)
    );
}

#[test]
fn the_daemon_carries_changes_as_the_command_itself_does() {
    let (direct, served) = (branches(), branches());
    change_apart(&direct);
    change_apart(&served);
    let said = ok(alt(&direct.alt, &["switch", "other"]));
    assert_eq!(ok(alt_daemon(&served.alt, &["switch", "other"])), said);
    assert_eq!(files(&served.alt), files(&direct.alt));
    assert_eq!(index(&served.alt), index(&direct.alt));
    assert_eq!(heads(&served.alt), heads(&direct.alt));
}

#[test]
fn a_parallel_workspace_carries_its_own_changes() {
    let pair = branches();
    ok(alt(&pair.alt, &["branch", "third"]));
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap(), "third"],
    ));
    std::fs::write(tree.join("b"), "b2\n").unwrap();
    std::fs::write(tree.join("c"), "c2\n").unwrap();
    ok(alt(&pair.alt, &["--workspace", "ws", "add", "c"]));
    let said = ok(alt(&pair.alt, &["--workspace", "ws", "switch", "other"]));
    assert_eq!(said, "M\tb\nM\tc\nSwitched to branch 'other'\n");

    // the same in git, in the repository's only tree
    std::fs::write(pair.git.join("b"), "b2\n").unwrap();
    std::fs::write(pair.git.join("c"), "c2\n").unwrap();
    ok(git(&pair.git, &["add", "c"]));
    ok(git(&pair.git, &["switch", "other"]));
    assert_eq!(files(&tree), files(&pair.git));
    let staged = index_at(&pair.alt, &pair.alt.join(".alt/workspaces/ws/index"));
    assert_eq!(staged, ok(git(&pair.git, &["ls-files", "-s"])));
    // the default workspace is where it was, with its files as they were
    assert!(ok(alt(&pair.alt, &["status"])).contains("On branch main"));
    assert_eq!(std::fs::read_to_string(pair.alt.join("b")).unwrap(), "b\n");
}
