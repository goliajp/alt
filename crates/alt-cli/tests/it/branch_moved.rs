//! A branch put at another commit or under another name: `alt branch -f`,
//! `-m`, `-M`, `alt switch -C` and `alt checkout -B`, each beside git's.

use crate::like_git::{Pair, alt, alt_daemon, fails, files, git, heads, index, ok};
use crate::of_late;

/// `main` with three commits that each change `a`, `side` at the second
/// and `other` at the first; `main` checked out.
fn three() -> Pair {
    let pair = Pair::new();
    for (content, message) in [("1\n", "first"), ("2\n", "second"), ("3\n", "third")] {
        pair.write("a", content);
        pair.commit_all(message);
    }
    pair.both(&["branch", "side", "HEAD~1"]);
    pair.both(&["branch", "other", "HEAD~2"]);
    pair
}

fn on(repo: &std::path::Path) -> String {
    ok(alt(repo, &["rev-parse", "--abbrev-ref", "HEAD"]))
}

#[test]
fn a_branch_is_put_where_force_names_it() {
    let pair = three();
    for args in [
        &["branch", "-f", "side", "HEAD~2"][..],
        &["branch", "-f", "other"],
        &["branch", "-f", "fresh", "HEAD~1"],
        &["branch", "--force", "side", "other"],
    ] {
        pair.both(args);
        assert_eq!(heads(&pair.alt), heads(&pair.git), "{args:?}");
    }
    let before = heads(&pair.alt);
    of_late::same_failure(
        &pair,
        &["branch", "-f", "side", "nosuch"],
        128,
        "fatal: not a valid object name: 'nosuch'\n",
    );
    // the branch checked out is not put elsewhere under its files
    let refused = fails(alt(&pair.alt, &["branch", "-f", "main", "HEAD~1"]), 128);
    let at = pair.alt.canonicalize().unwrap();
    let said = "fatal: cannot force update the branch 'main' used by worktree at";
    assert!(
        refused.starts_with(&format!("{said} '{}'\n", at.display())),
        "{refused}"
    );
    let theirs = fails(git(&pair.git, &["branch", "-f", "main", "HEAD~1"]), 128);
    assert!(theirs.starts_with(said), "{theirs}");
    assert_eq!(heads(&pair.alt), before);
}

#[test]
fn a_branch_put_elsewhere_is_back_after_an_undo() {
    let pair = three();
    let before = heads(&pair.alt);
    ok(alt(&pair.alt, &["branch", "-f", "side", "HEAD~2"]));
    assert_ne!(heads(&pair.alt), before);
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(heads(&pair.alt), before);
    ok(alt(&pair.alt, &["branch", "-M", "side", "other"]));
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(heads(&pair.alt), before);
    ok(alt(&pair.alt, &["branch", "-m", "renamed"]));
    assert_eq!(on(&pair.alt), "renamed\n");
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(heads(&pair.alt), before);
    ok(alt(&pair.alt, &["switch", "-C", "side", "other"]));
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "1\n");
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(heads(&pair.alt), before);
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "3\n");
}

#[test]
fn a_branch_takes_another_name_with_the_head_on_it() {
    let pair = three();
    for args in [
        &["branch", "-m", "side", "moved"][..],
        &["branch", "-m", "renamed"],
        &["branch", "-m", "renamed", "renamed"],
        &["branch", "-M", "moved", "other"],
        &["branch", "--move", "other", "last"],
    ] {
        pair.both(args);
        assert_eq!(heads(&pair.alt), heads(&pair.git), "{args:?}");
    }
    assert_eq!(on(&pair.alt), "renamed\n");
    let before = heads(&pair.alt);
    pair.both(&["branch", "again", "HEAD~1"]);
    for (args, said) in [
        (
            &["branch", "-m", "nosuch", "x"][..],
            "fatal: no branch named 'nosuch'\n",
        ),
        (
            &["branch", "-m", "last", "again"],
            "fatal: a branch named 'again' already exists\n",
        ),
    ] {
        of_late::same_failure(&pair, args, 128, said);
    }
    pair.both(&["branch", "-d", "again"]);
    assert_eq!(heads(&pair.alt), before);
    // -M over the branch checked out is refused as -f is
    let refused = fails(alt(&pair.alt, &["branch", "-M", "last", "renamed"]), 128);
    assert!(refused.starts_with("fatal: cannot force update the branch 'renamed' used by"));
    fails(git(&pair.git, &["branch", "-M", "last", "renamed"]), 128);
    // what the branch follows goes with it
    ok(alt(&pair.alt, &["config", "branch.last.remote", "origin"]));
    ok(alt(
        &pair.alt,
        &["config", "branch.last.merge", "refs/heads/x"],
    ));
    ok(alt(&pair.alt, &["branch", "-m", "last", "final"]));
    assert_eq!(
        ok(alt(&pair.alt, &["config", "branch.final.remote"])),
        "origin\n"
    );
    assert_eq!(
        ok(alt(&pair.alt, &["config", "branch.final.merge"])),
        "refs/heads/x\n"
    );
    fails(alt(&pair.alt, &["config", "branch.last.remote"]), 128);
}

#[test]
fn a_switch_puts_the_branch_it_goes_to_where_it_is_told() {
    for (word, flag) in [("switch", "-C"), ("checkout", "-B")] {
        let pair = three();
        // a change the commits agree on goes along
        pair.write("n", "new\n");
        pair.both(&["add", "n"]);
        for (args, said) in [
            (
                &[word, flag, "side", "other"][..],
                "Switched to and reset branch 'side'",
            ),
            (&[word, flag, "side", "main"], "Reset branch 'side'"),
            (
                &[word, flag, "fresh", "main~1"],
                "Switched to a new branch 'fresh'",
            ),
            (
                &[word, flag, "other"],
                "Switched to and reset branch 'other'",
            ),
        ] {
            let (ours, theirs) = pair.run(args);
            let told = String::from_utf8_lossy(&theirs.stderr).into_owned();
            assert!(told.contains(said), "{args:?}: {told}");
            assert!(ok(ours).contains(said), "{args:?}");
            ok(theirs);
            pair.assert_same_state();
        }
        // a change the move would write over refuses it, and nothing moves
        pair.write("a", "mine\n");
        let before = (heads(&pair.alt), files(&pair.alt), index(&pair.alt));
        let said = pair.same_failure(&[word, flag, "side", "main~2"], 1);
        assert!(said.contains("would be overwritten by checkout"), "{said}");
        assert_eq!(
            (heads(&pair.alt), files(&pair.alt), index(&pair.alt)),
            before
        );
        let nowhere = match word {
            "switch" => "fatal: invalid reference: nosuch\n",
            _ => "fatal: 'nosuch' is not a commit and a branch 'side' cannot be created from it\n",
        };
        of_late::same_failure(&pair, &[word, flag, "side", "nosuch"], 128, nowhere);
    }
}

#[test]
fn a_branch_another_workspace_is_on_stays_under_it() {
    let (direct, served) = (three(), three());
    for args in [
        &["branch", "-f", "side", "other"][..],
        &["branch", "-m", "side", "moved"],
        &["switch", "-C", "moved", "main~1"],
        &["checkout", "-B", "fresh", "other"],
        &["branch", "-M", "moved", "other"],
    ] {
        assert_eq!(
            ok(alt_daemon(&served.alt, args)),
            ok(alt(&direct.alt, args)),
            "{args:?}"
        );
        assert_eq!(files(&served.alt), files(&direct.alt));
        assert_eq!(index(&served.alt), index(&direct.alt));
        assert_eq!(heads(&served.alt), heads(&direct.alt));
    }
    let json = ok(alt(
        &direct.alt,
        &["switch", "--json", "-C", "main", "other"],
    ));
    assert!(json.contains("\"branch\":\"main\"") && json.contains("\"result\":\"reset\""));

    let pair = three();
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap()],
    ));
    ok(alt(&tree, &["switch", "side"]));
    let before = (heads(&pair.alt), files(&tree));
    let at = tree.canonicalize().unwrap();
    let held = format!("used by worktree at '{}'\n", at.display());
    let refused = fails(alt(&pair.alt, &["branch", "-f", "side", "main"]), 128);
    assert!(refused.starts_with("fatal: cannot force update the branch 'side'"));
    assert!(refused.contains(&held), "{refused}");
    let refused = fails(alt(&pair.alt, &["switch", "-C", "side", "main"]), 128);
    assert!(
        refused.starts_with(&format!("fatal: 'side' is already {held}")),
        "{refused}"
    );
    let refused = fails(alt(&pair.alt, &["branch", "-M", "other", "side"]), 128);
    assert!(refused.contains(&held), "{refused}");
    assert_eq!((heads(&pair.alt), files(&tree)), before);
    // under another name it is still what that workspace is on
    ok(alt(&pair.alt, &["branch", "-m", "side", "theirs"]));
    assert_eq!(on(&tree), "theirs\n");
    assert_eq!(on(&pair.alt), "main\n");
    assert_eq!(files(&tree), before.1);
    assert!(!ok(alt(&tree, &["status", "--json"])).contains("\"clean\":false"));
    // and the workspace itself puts a branch where it stands
    ok(alt(&tree, &["switch", "-C", "mine", "main"]));
    assert_eq!(std::fs::read_to_string(tree.join("a")).unwrap(), "3\n");
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "3\n");
    assert_eq!(on(&tree), "mine\n");
}
