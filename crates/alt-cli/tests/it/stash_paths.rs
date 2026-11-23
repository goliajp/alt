//! `alt stash push` of the paths named and no other, beside git's.

use crate::like_git::{Pair, alt, alt_daemon, fails, files, git, index, ok};
use crate::of_late;
use crate::stash::assert_same_stack;

/// A tree with every kind of change in it: files changed, one of them
/// staged and changed again, a new file staged, a file gone, one the
/// index lost, and files not tracked.
fn messy() -> Pair {
    let pair = Pair::new();
    for name in ["a", "b", "d/x", "d/y", "gone", "lost"] {
        pair.write(name, "1\n");
    }
    pair.commit_all("base commit");
    for name in ["a", "b", "d/x"] {
        pair.write(name, "2\n");
    }
    pair.write("d/y", "staged\n");
    pair.write("staged_new", "new\n");
    pair.both(&["add", "d/y", "staged_new"]);
    pair.write("d/y", "3\n");
    pair.write("un", "u\n");
    pair.write("d/un", "u\n");
    pair.remove("gone");
    pair.both(&["rm", "-q", "--cached", "lost"]);
    pair.write("lost", "w\n");
    pair
}

/// git's words for a path given that names no tracked file: `path` with
/// how much of it is where the command runs before it.
fn unknown(path: &str) -> String {
    format!(
        "error: pathspec ':(prefix:{path}' did not match any file(s) known to git\n\
         Did you forget to 'git add'?\n"
    )
}

#[test]
fn the_paths_named_are_put_away_and_the_rest_stays() {
    for push in [
        &["stash", "push", "--", "a", "d"][..],
        &["stash", "push", "-u", "-m", "named", "--", "d", "un"],
        &["stash", "--", "a"],
        &["stash", "push", "a", "b"],
        &["stash", "push", "--", "staged_new"],
        &["stash", "push", "--", "gone"],
        &["stash", "push", "--", "d/*"],
        &["stash", "push", "-q", "--", "a"],
        &["stash", "push", "-u", "--", "lost"],
    ] {
        let pair = messy();
        let said = pair.same(push);
        assert_eq!(said.is_empty(), push.contains(&"-q"), "{push:?}: {said}");
        pair.assert_same_state();
        assert_same_stack(&pair);
        pair.same(&["stash", "show"]);
        // what was put away comes back; a file the index had lost, put
        // away with the untracked ones, is in its own way then
        let (ours, theirs) = pair.run(&["stash", "pop"]);
        let lost = push.contains(&"lost");
        of_late::same_status(
            &ours,
            &theirs,
            if lost { 128 } else { 0 },
            &format!("{push:?}"),
        );
        pair.assert_same_state();
        assert_same_stack(&pair);
    }
}

/// git stops halfway here, the entry made and every change left staged:
/// it cannot write the file back where the one the index lost still is.
#[test]
fn a_file_the_index_lost_among_the_paths_is_put_away_and_comes_back() {
    let pair = messy();
    let before = files(&pair.alt);
    ok(alt(&pair.alt, &["stash", "push", "--", "."]));
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("lost")).unwrap(),
        "1\n"
    );
    assert_eq!(std::fs::read_to_string(pair.alt.join("un")).unwrap(), "u\n");
    let staged = index(&pair.alt);
    assert!(
        staged.contains("\tlost\n") && !staged.contains("staged_new"),
        "{staged}"
    );
    ok(alt(&pair.alt, &["stash", "pop"]));
    assert_eq!(files(&pair.alt), before);
}

#[test]
fn a_path_is_read_from_where_the_command_runs() {
    let pair = messy();
    let args = ["stash", "push", "--", "../b", "x"];
    let said = ok(alt(&pair.alt.join("d"), &args));
    assert_eq!(said, ok(git(&pair.git.join("d"), &args)));
    pair.assert_same_state();
    assert_same_stack(&pair);
    assert_eq!(std::fs::read_to_string(pair.alt.join("b")).unwrap(), "1\n");
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "2\n");
    let (ours, theirs) = (pair.alt.join("d"), pair.git.join("d"));
    // git says how much of the path is where the command runs
    for (path, said) in [("nope", "2)d/nope"), ("../nope", "0)nope")] {
        let args = ["stash", "push", "--", path];
        of_late::same_failure_at((&ours, &theirs), &args, 128, &unknown(said));
    }
}

#[test]
fn a_path_that_names_nothing_tracked_is_refused_and_one_unchanged_is_nothing_to_save() {
    let pair = messy();
    let (before, staged) = (files(&pair.alt), index(&pair.alt));
    for path in ["nope", "un", "lost"] {
        let said = unknown(&format!("0){path}"));
        of_late::same_failure(&pair, &["stash", "push", "--", path], 128, &said);
        of_late::same_failure(&pair, &["stash", "push", "--", "a", path], 128, &said);
    }
    let said = "fatal: subcommand wasn't specified; 'push' can't be assumed due to unexpected \
                token 'a'\n";
    assert_eq!(pair.same_failure(&["stash", "a"], 128), said);
    // with the untracked files asked for, a path may name one of those
    assert_eq!(
        pair.same(&["stash", "push", "-u", "--", "nope"]),
        "No local changes to save\n"
    );
    pair.both(&["restore", "a"]);
    assert_eq!(
        pair.same(&["stash", "push", "--", "a"]),
        "No local changes to save\n"
    );
    pair.write("a", "2\n");
    assert_eq!((files(&pair.alt), index(&pair.alt)), (before, staged));
    pair.assert_same_state();
    assert_eq!(ok(alt(&pair.alt, &["stash", "list"])), "");
}

#[test]
fn hunks_picked_one_by_one_are_said_not_to_be_supported() {
    let pair = messy();
    let (before, staged) = (files(&pair.alt), index(&pair.alt));
    for args in [
        &["stash", "-p"][..],
        &["stash", "push", "-p"],
        &["stash", "push", "--patch", "--", "a"],
    ] {
        let said = fails(alt(&pair.alt, args), 2);
        assert!(said.contains("stash push -p is not supported"), "{said}");
    }
    assert_eq!((files(&pair.alt), index(&pair.alt)), (before, staged));
    assert_eq!(ok(alt(&pair.alt, &["stash", "list"])), "");
}

#[test]
fn a_push_of_paths_is_undone_whole() {
    let pair = messy();
    let (before, staged) = (files(&pair.alt), index(&pair.alt));
    ok(alt(&pair.alt, &["stash", "push", "-u", "--", "a", "d"]));
    assert_ne!(files(&pair.alt), before);
    assert_eq!(std::fs::read_to_string(pair.alt.join("b")).unwrap(), "2\n");
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!((files(&pair.alt), index(&pair.alt)), (before, staged));
    assert_eq!(ok(alt(&pair.alt, &["stash", "list"])), "");
}

#[test]
fn the_daemon_and_a_parallel_workspace_put_paths_away_alike() {
    let (direct, served) = (messy(), messy());
    for args in [
        &["stash", "push", "-u", "--", "d", "un"][..],
        &["stash", "push", "-m", "one file", "--", "a"],
        &["stash", "list"],
    ] {
        assert_eq!(
            ok(alt_daemon(&served.alt, args)),
            ok(alt(&direct.alt, args)),
            "{args:?}"
        );
        assert_eq!(files(&served.alt), files(&direct.alt));
        assert_eq!(index(&served.alt), index(&direct.alt));
    }
    let json = ok(alt(&direct.alt, &["stash", "push", "--json", "--", "b"]));
    assert!(json.contains("\"saved\":\""), "{json}");

    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    let at = tree.to_str().unwrap();
    ok(alt(&direct.alt, &["workspace", "add", "ws", at]));
    std::fs::write(tree.join("a"), "ws a\n").unwrap();
    std::fs::write(tree.join("b"), "ws b\n").unwrap();
    let mine = files(&direct.alt);
    ok(alt(&tree, &["stash", "push", "--", "a"]));
    assert_eq!(std::fs::read_to_string(tree.join("a")).unwrap(), "1\n");
    assert_eq!(std::fs::read_to_string(tree.join("b")).unwrap(), "ws b\n");
    assert_eq!(files(&direct.alt), mine);
}
