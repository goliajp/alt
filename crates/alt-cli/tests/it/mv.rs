//! `alt mv` beside git: files and directories under a new name, in the
//! index and in the tree, and what it refuses.

use crate::like_git::{Pair, alt, alt_daemon, fails, files, git, index, index_at, ok};
use crate::of_late;

fn tree() -> Pair {
    let pair = Pair::new();
    for name in ["a", "b", "c", "g", "f", "d/x", "d/s/y"] {
        pair.write(name, &format!("{name}\n"));
    }
    for root in [&pair.alt, &pair.git] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.join("f"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("a", root.join("link")).unwrap();
        std::fs::create_dir(root.join("e")).unwrap();
    }
    pair.commit_all("base");
    pair
}

#[test]
fn a_file_is_renamed_or_moved_into_a_directory() {
    let pair = tree();
    assert_eq!(pair.same(&["mv", "a", "a2"]), "");
    pair.same(&["mv", "b", "d"]);
    pair.same(&["mv", "c", "d/"]);
    pair.same(&["mv", "f", "link", "e"]);
    pair.assert_same_state();
    assert!(pair.alt.join("d/b").exists() && !pair.alt.join("b").exists());
}

#[test]
fn a_directory_goes_with_all_that_is_in_it() {
    let pair = tree();
    pair.write("d/untracked", "u\n");
    pair.write("d/x", "x2\n");
    pair.same(&["mv", "d", "dd"]);
    pair.assert_same_state();
    assert!(!pair.alt.join("d").exists());
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("dd/untracked")).unwrap(),
        "u\n"
    );
    pair.same(&["mv", "dd", "e"]);
    pair.assert_same_state();
    assert!(pair.alt.join("e/dd/s/y").exists());
}

#[test]
fn changes_staged_and_not_go_along() {
    let pair = tree();
    pair.write("a", "staged\n");
    pair.both(&["add", "a"]);
    pair.write("a", "later\n");
    pair.same(&["mv", "a", "moved"]);
    pair.assert_same_state();
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("moved")).unwrap(),
        "later\n"
    );
}

#[test]
fn a_path_is_named_from_the_directory_the_command_runs_in() {
    let pair = tree();
    ok(alt(&pair.alt.join("d"), &["mv", "x", "../top"]));
    ok(git(&pair.git.join("d"), &["mv", "x", "../top"]));
    ok(alt(&pair.alt.join("d"), &["mv", "../a", "."]));
    ok(git(&pair.git.join("d"), &["mv", "../a", "."]));
    pair.assert_same_state();
    assert!(pair.alt.join("top").exists() && pair.alt.join("d/a").exists());
}

#[test]
fn what_cannot_be_moved_is_refused_as_git_refuses_it() {
    let pair = tree();
    pair.write("u", "u\n");
    for (args, said) in [
        (
            &["mv", "nope", "x"][..],
            "bad source, source=nope, destination=x",
        ),
        (
            &["mv", "u", "u2"],
            "not under version control, source=u, destination=u2",
        ),
        (
            &["mv", "g", "f"],
            "destination exists, source=g, destination=f",
        ),
        (
            &["mv", "a", "d/s/y"],
            "destination exists, source=a, destination=d/s/y",
        ),
        (
            &["mv", "d", "d/sub"],
            "can not move directory into itself, source=d, destination=d/sub",
        ),
        (
            &["mv", "d", "d"],
            "can not move directory into itself, source=d, destination=d/d",
        ),
        (&["mv", "a", "b", "c"], "destination 'c' is not a directory"),
    ] {
        let refused = pair.same_failure(args, 128);
        assert_eq!(refused, format!("fatal: {said}\n"), "{args:?}");
    }
    let said = "fatal: destination directory does not exist, source=a, destination=nodir/a3\n";
    of_late::same_failure(&pair, &["mv", "a", "nodir/a3"], 128, said);
    pair.write("e/d/z", "z\n");
    let refused = pair.same_failure(&["mv", "d", "e"], 128);
    assert_eq!(
        refused,
        "fatal: destination already exists, source=d, destination=e/d\n"
    );
    pair.remove("e/d/z");
    pair.assert_same_state();
    fails(alt(&pair.alt, &["mv", "a"]), 2);

    // with -f a file at the new name is written over, with -k a source
    // that cannot be moved is passed over
    pair.same(&["mv", "-f", "g", "f"]);
    pair.same(&["mv", "-k", "nope", "a", "e"]);
    pair.assert_same_state();
    assert_eq!(std::fs::read_to_string(pair.alt.join("f")).unwrap(), "g\n");
}

#[test]
fn renames_are_said_when_asked_and_only_said_in_a_dry_run() {
    let pair = tree();
    let said = pair.same(&["mv", "-n", "a", "zzz"]);
    assert_eq!(said, "Checking rename of 'a' to 'zzz'\nRenaming a to zzz\n");
    pair.assert_same_state();
    assert!(pair.alt.join("a").exists());
    assert_eq!(pair.same(&["mv", "-v", "a", "zzz"]), "Renaming a to zzz\n");
    let said = pair.same(&["mv", "-v", "d", "dd"]);
    assert_eq!(
        said,
        "Renaming d to dd\nRenaming d/s/y to dd/s/y\nRenaming d/x to dd/x\n"
    );
    pair.same(&["mv", "-n", "dd", "b", "e"]);
    pair.assert_same_state();
}

#[test]
fn a_move_is_undone() {
    let pair = tree();
    pair.write("d/untracked", "u\n");
    pair.write("a", "changed\n");
    let before = (files(&pair.alt), index(&pair.alt));
    ok(alt(&pair.alt, &["mv", "d", "a", "e"]));
    assert_ne!(files(&pair.alt), before.0);
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!((files(&pair.alt), index(&pair.alt)), before);
}

#[test]
fn the_daemon_moves_as_the_command_itself_does() {
    let (direct, served) = (tree(), tree());
    let args = ["mv", "-v", "d", "a", "e"];
    assert_eq!(
        ok(alt_daemon(&served.alt, &args)),
        ok(alt(&direct.alt, &args))
    );
    assert_eq!(files(&served.alt), files(&direct.alt));
    assert_eq!(index(&served.alt), index(&direct.alt));
    let json = ok(alt_daemon(&served.alt, &["mv", "--json", "b", "b2"]));
    assert!(
        json.contains("\"renamed\":[{\"from\":\"b\",\"to\":\"b2\"}]"),
        "{json}"
    );
}

#[test]
fn a_parallel_workspace_moves_its_own_files() {
    let pair = tree();
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap()],
    ));
    ok(alt(&tree, &["mv", "d", "dd"]));
    ok(alt(&pair.alt, &["--workspace", "ws", "mv", "a", "a2"]));
    ok(git(&pair.git, &["mv", "d", "dd"]));
    ok(git(&pair.git, &["mv", "a", "a2"]));
    let staged = index_at(&pair.alt, &pair.alt.join(".alt/workspaces/ws/index"));
    assert_eq!(staged, ok(git(&pair.git, &["ls-files", "-s"])));
    assert!(tree.join("dd/x").exists() && tree.join("a2").exists());
    assert!(pair.alt.join("d/x").exists() && pair.alt.join("a").exists());
}
