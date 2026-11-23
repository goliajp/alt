//! `alt stash` beside git: what an entry is made of, what comes back, and
//! the stack as an exported repository has it.

use std::path::Path;

use crate::like_git::{Pair, alt, alt_daemon, export, fails, files, git, heads, index, ok};

fn tree() -> Pair {
    let pair = Pair::new();
    for name in ["a", "b", "c"] {
        pair.write(name, &format!("{name}\n"));
    }
    pair.commit_all("base commit");
    pair
}

/// A change not staged, one staged, a new file staged, a file deleted
/// and a file not tracked.
fn dirty(pair: &Pair) {
    pair.write("a", "a2\n");
    pair.write("b", "b2\n");
    pair.write("n", "n\n");
    pair.both(&["add", "b", "n"]);
    pair.remove("c");
    pair.write("u", "u\n");
}

/// The entries of a git repository, the latest first: each with the id of
/// its commit and what it is called.
fn stack(repo: &Path) -> String {
    ok(git(repo, &["stash", "list", "--format=%H %gd %gs"]))
}

/// The stack of an alt repository, as git reads it from an export.
fn exported_stack(repo: &Path) -> String {
    stack(&export(repo).path().join("exported"))
}

/// The stacks are the same in both: the same commits, called the same.
fn assert_same_stack(pair: &Pair) {
    assert_eq!(exported_stack(&pair.alt), stack(&pair.git));
}

#[test]
fn a_push_makes_the_commits_git_makes_and_cleans_the_tree() {
    let pair = tree();
    dirty(&pair);
    let said = pair.same(&["stash"]);
    assert!(
        said.starts_with("Saved working directory and index state WIP on main: "),
        "{said}"
    );
    assert!(said.ends_with(" base commit\n"), "{said}");
    pair.assert_same_state();
    assert_same_stack(&pair);
    assert!(pair.alt.join("u").exists() && pair.alt.join("c").exists());
    assert_eq!(
        pair.same(&["stash", "list"]),
        said.replace("Saved working directory and index state ", "stash@{0}: ")
    );
    assert_eq!(pair.same(&["stash"]), "No local changes to save\n");
}

#[test]
fn a_push_with_a_message_and_the_untracked_files() {
    let pair = tree();
    dirty(&pair);
    let said = pair.same(&["stash", "push", "-m", "second one", "-u"]);
    assert_eq!(
        said,
        "Saved working directory and index state On main: second one\n"
    );
    pair.assert_same_state();
    assert!(!pair.alt.join("u").exists());
    assert_same_stack(&pair);
    // an untracked file alone is something to save
    pair.write("only", "o\n");
    pair.same(&["stash", "-u"]);
    assert_same_stack(&pair);
    assert_eq!(pair.same(&["stash", "list"]).lines().count(), 2);
}

#[test]
fn an_apply_brings_the_changes_back_and_keeps_the_entry() {
    let pair = tree();
    dirty(&pair);
    pair.both(&["stash", "-u"]);
    pair.both(&["stash", "apply"]);
    pair.assert_same_state();
    assert_same_stack(&pair);
    assert_eq!(std::fs::read_to_string(pair.alt.join("u")).unwrap(), "u\n");
    assert!(!pair.alt.join("c").exists());
}

#[test]
fn a_pop_takes_the_entry_out_of_the_stack() {
    let pair = tree();
    dirty(&pair);
    pair.both(&["stash"]);
    pair.write("a", "later\n");
    pair.both(&["add", "a"]);
    pair.both(&["stash", "-m", "later"]);
    let popped = ok(alt(&pair.alt, &["stash", "pop", "stash@{1}"]));
    let git_popped = ok(git(&pair.git, &["stash", "pop", "stash@{1}"]));
    assert_eq!(popped.lines().last(), git_popped.lines().last());
    assert!(
        popped
            .lines()
            .last()
            .unwrap()
            .starts_with("Dropped stash@{1} ("),
        "{popped}"
    );
    pair.assert_same_state();
    assert_same_stack(&pair);
    assert_eq!(pair.same(&["stash", "list"]), "stash@{0}: On main: later\n");
}

#[test]
fn what_was_staged_comes_back_staged_with_index() {
    let pair = tree();
    dirty(&pair);
    pair.both(&["stash"]);
    pair.both(&["stash", "pop", "--index"]);
    pair.assert_same_state();
    assert_eq!(pair.same(&["stash", "list"]), "");
}

#[test]
fn a_pop_that_conflicts_leaves_the_conflict_and_the_entry() {
    let pair = tree();
    pair.write("a", "stashed\n");
    pair.both(&["stash"]);
    pair.write("a", "committed\n");
    pair.commit_all("moved on");
    let (popped, git_popped) = pair.run(&["stash", "pop"]);
    assert_eq!(
        (popped.status.code(), git_popped.status.code()),
        (Some(1), Some(1))
    );
    let said = String::from_utf8(popped.stdout).unwrap();
    assert!(
        said.contains("CONFLICT (content): Merge conflict in a\n"),
        "{said}"
    );
    assert!(
        said.ends_with("The stash entry is kept in case you need it again.\n"),
        "{said}"
    );
    pair.assert_same_state();
    assert_same_stack(&pair);
    let left = std::fs::read_to_string(pair.alt.join("a")).unwrap();
    assert!(left.contains("<<<<<<< Updated upstream") && left.contains(">>>>>>> Stashed changes"));
}

#[test]
fn an_apply_over_a_change_that_would_be_lost_is_refused() {
    let pair = tree();
    pair.write("a", "stashed\n");
    pair.write("u", "u\n");
    pair.both(&["stash", "-u"]);
    pair.write("a", "mine\n");
    let (applied, git_applied) = pair.run(&["stash", "apply"]);
    assert!(!applied.status.success() && !git_applied.status.success());
    let said = String::from_utf8(applied.stderr).unwrap();
    assert!(
        said.contains(
            "error: Your local changes to the following files would be overwritten by merge:\n\ta\n"
        ),
        "{said}"
    );
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("a")).unwrap(),
        "mine\n"
    );

    // an untracked file of the entry does not go where a file is
    pair.both(&["restore", "a"]);
    pair.write("u", "in the way\n");
    let (applied, git_applied) = pair.run(&["stash", "apply"]);
    assert!(!applied.status.success() && !git_applied.status.success());
    let said = String::from_utf8(applied.stderr).unwrap();
    assert!(said.contains("u already exists, no checkout\n"), "{said}");
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "a\n");
    assert_same_stack(&pair);
}

#[test]
fn entries_are_dropped_by_name_and_all_at_once() {
    let pair = tree();
    for name in ["one", "two", "three"] {
        pair.write("a", &format!("{name}\n"));
        pair.both(&["stash", "-m", name]);
    }
    let dropped = pair.same(&["stash", "drop", "stash@{1}"]);
    assert!(dropped.starts_with("Dropped stash@{1} ("), "{dropped}");
    assert_same_stack(&pair);
    let dropped = pair.same(&["stash", "drop"]);
    assert!(dropped.starts_with("Dropped refs/stash@{0} ("), "{dropped}");
    assert_same_stack(&pair);
    assert_eq!(pair.same(&["stash", "list"]), "stash@{0}: On main: one\n");
    pair.same_failure(&["stash", "drop", "4"], 128);
    pair.same_failure(&["stash", "apply", "stash@{4}"], 128);
    pair.same(&["stash", "clear"]);
    assert_eq!(exported_stack(&pair.alt), "");
    // git ends with 128 here since 2.5x, with 1 before
    for verb in ["pop", "drop", "apply"] {
        let none = fails(alt(&pair.alt, &["stash", verb]), 128);
        assert!(none.starts_with("No stash entries found.\n"), "{none}");
        let git_none = git(&pair.git, &["stash", verb]);
        assert!(!git_none.status.success());
        assert_eq!(
            String::from_utf8_lossy(&git_none.stderr),
            "No stash entries found.\n"
        );
    }
}

#[test]
fn a_repository_without_a_commit_has_nothing_to_stash_onto() {
    let pair = Pair::new();
    pair.write("a", "a\n");
    pair.both(&["add", "a"]);
    let said = pair.same_failure(&["stash"], 1);
    assert_eq!(said, "You do not have the initial commit yet\n");
}

#[test]
fn git_pops_an_entry_from_an_exported_repository() {
    let pair = tree();
    dirty(&pair);
    ok(alt(&pair.alt, &["stash", "-u"]));
    pair.write("a", "later\n");
    ok(alt(&pair.alt, &["stash", "-m", "later"]));
    let out = export(&pair.alt);
    let exported = out.path().join("exported");
    ok(git(&exported, &["reset", "-q", "--hard"]));
    assert_eq!(ok(git(&exported, &["stash", "list"])).lines().count(), 2);
    ok(git(&exported, &["stash", "drop", "-q"]));
    ok(git(&exported, &["stash", "pop", "-q"]));
    assert_eq!(std::fs::read_to_string(exported.join("a")).unwrap(), "a2\n");
    assert_eq!(std::fs::read_to_string(exported.join("u")).unwrap(), "u\n");
    assert!(!exported.join("c").exists());
    assert_eq!(ok(git(&exported, &["stash", "list"])), "");
    assert!(git(&exported, &["fsck", "--strict"]).status.success());
}

#[test]
fn a_push_and_a_pop_are_undone() {
    let pair = tree();
    dirty(&pair);
    let before = (files(&pair.alt), index(&pair.alt), heads(&pair.alt));
    ok(alt(&pair.alt, &["stash", "-u"]));
    let stashed = (
        files(&pair.alt),
        index(&pair.alt),
        exported_stack(&pair.alt),
    );
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(
        (files(&pair.alt), index(&pair.alt), heads(&pair.alt)),
        before
    );
    assert_eq!(exported_stack(&pair.alt), "");

    ok(alt(&pair.alt, &["stash", "-u"]));
    ok(alt(&pair.alt, &["stash", "pop"]));
    assert_eq!(exported_stack(&pair.alt), "");
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(
        (
            files(&pair.alt),
            index(&pair.alt),
            exported_stack(&pair.alt)
        ),
        stashed
    );
}

#[test]
fn the_daemon_stashes_as_the_command_itself_does() {
    let (direct, served) = (tree(), tree());
    dirty(&direct);
    dirty(&served);
    for args in [&["stash", "-u"][..], &["stash", "list"], &["stash", "pop"]] {
        assert_eq!(
            ok(alt_daemon(&served.alt, args)),
            ok(alt(&direct.alt, args)),
            "{args:?}"
        );
        assert_eq!(files(&served.alt), files(&direct.alt));
        assert_eq!(index(&served.alt), index(&direct.alt));
        assert_eq!(exported_stack(&served.alt), exported_stack(&direct.alt));
    }
    let json = ok(alt_daemon(&served.alt, &["stash", "list", "--json"]));
    assert!(json.contains("\"entries\":[]"), "{json}");
}

#[test]
fn a_parallel_workspace_stashes_its_own_changes_onto_the_shared_stack() {
    let pair = tree();
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap()],
    ));
    std::fs::write(tree.join("a"), "ws\n").unwrap();
    std::fs::write(pair.alt.join("b"), "default\n").unwrap();
    let said = ok(alt(&pair.alt, &["--workspace", "ws", "stash"]));
    assert!(said.contains("WIP on ws: "), "{said}");
    assert_eq!(std::fs::read_to_string(tree.join("a")).unwrap(), "a\n");
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("b")).unwrap(),
        "default\n"
    );
    // the stack is the repository's: the entry is seen from either tree
    assert_eq!(
        ok(alt(&pair.alt, &["stash", "list"])),
        ok(alt(&tree, &["stash", "list"]))
    );
    ok(alt(&tree, &["stash", "pop"]));
    assert_eq!(std::fs::read_to_string(tree.join("a")).unwrap(), "ws\n");
    assert_eq!(ok(alt(&pair.alt, &["stash", "list"])), "");
    fails(alt(&pair.alt, &["--workspace", "ws", "stash", "pop"]), 128);
}

#[test]
fn the_stack_of_a_git_repository_is_imported_from_its_log() {
    let pair = tree();
    for name in ["one", "two", "three"] {
        std::fs::write(pair.git.join("a"), format!("{name}\n")).unwrap();
        ok(git(&pair.git, &["stash", "-m", name]));
    }
    ok(git(&pair.git, &["stash", "drop", "stash@{1}"]));
    let out = alt_os::temp::tempdir().unwrap();
    let imported = out.path().join("imported");
    ok(alt(&pair.git, &["import", imported.to_str().unwrap()]));
    let listed = ok(alt(&imported, &["stash", "list"]));
    assert_eq!(listed, ok(git(&pair.git, &["stash", "list"])));
    assert_eq!(
        listed,
        "stash@{0}: On main: three\nstash@{1}: On main: one\n"
    );
    assert_eq!(exported_stack(&imported), stack(&pair.git));
    ok(alt(&imported, &["stash", "pop"]));
    assert_eq!(
        std::fs::read_to_string(imported.join("a")).unwrap(),
        "three\n"
    );

    // an entry git has no longer goes with the next import
    ok(git(&pair.git, &["stash", "drop"]));
    ok(alt(&imported, &["restore", "a"]));
    ok(alt(&pair.git, &["import", imported.to_str().unwrap()]));
    assert_eq!(
        ok(alt(&imported, &["stash", "list"])),
        "stash@{0}: On main: one\n"
    );
}
