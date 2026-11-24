//! A branch a rebase or a bisect took HEAD off is held by its workspace
//! still: it is not put elsewhere, renamed, deleted or checked out
//! by another, as git refuses for the worktree that rebases or bisects.

use std::path::Path;

use crate::like_git::{Pair, alt, fails, git, heads, ok};
use crate::of_late;

/// `main` gone one commit on from where `topic`, with two commits of its
/// own, left it; `other` on the first commit. `topic` checked out.
fn history() -> Pair {
    let pair = Pair::new();
    pair.write("f", "1\n");
    pair.commit_all("base");
    pair.both(&["branch", "topic"]);
    pair.both(&["branch", "other"]);
    pair.write("g", "2\n");
    pair.commit_all("main moves on");
    pair.both(&["switch", "topic"]);
    pair.write("a", "a\n");
    pair.commit_all("topic one");
    pair.write("b", "b\n");
    pair.commit_all("topic two");
    pair
}

/// Where the branches are, read while a rebase or a bisect has HEAD.
fn branches(repo: &Path) -> String {
    ok(alt(repo, &["rev-parse", "main", "topic", "other"]))
}

/// The command is refused by both with `code`, saying `said` and then
/// where the branch is held.
fn held(pair: &Pair, args: &[&str], code: i32, said: &str) {
    let ours = fails(alt(&pair.alt, args), code);
    let at = pair.alt.canonicalize().unwrap();
    assert!(ours.starts_with(said), "{args:?}: {ours}");
    assert!(ours.contains(&at.display().to_string()), "{args:?}: {ours}");
    let theirs = fails(git(&pair.git, args), code);
    assert!(theirs.starts_with(said), "{args:?}: {theirs}");
}

/// A working tree of `topic` beside the one that has it is refused by
/// both in git's words, and none is made.
fn no_second_tree(pair: &Pair) {
    let trees = alt_os::temp::tempdir().unwrap();
    let said = "fatal: 'topic' is already used by worktree at '";
    let tree = trees.path().join("theirs");
    let theirs = git(
        &pair.git,
        &["worktree", "add", tree.to_str().unwrap(), "topic"],
    );
    assert!(fails(theirs, 128).contains(said));
    let tree = trees.path().join("ours");
    let args = [
        "workspace",
        "add",
        "second",
        tree.to_str().unwrap(),
        "topic",
    ];
    let ours = fails(alt(&pair.alt, &args), 128);
    let at = pair.alt.canonicalize().unwrap();
    assert!(
        ours.starts_with(&format!("{said}{}'\n", at.display())),
        "{ours}"
    );
    assert!(!tree.join(".alt").exists());
    assert!(!ok(alt(&pair.alt, &["workspace", "list"])).contains("second"));
}

#[test]
fn a_branch_being_rebased_or_bisected_stays_as_it_is() {
    let pair = history();
    let (a, g) = pair.run(&["rebase", "main", "-x", "false"]);
    fails(a, 1);
    fails(g, 1);
    let before = branches(&pair.alt);
    let forced = "fatal: cannot force update the branch 'topic' used by worktree at '";
    held(&pair, &["branch", "-f", "topic", "main"], 128, forced);
    held(&pair, &["branch", "-M", "other", "topic"], 128, forced);
    let renamed = "fatal: branch refs/heads/topic is being rebased at ";
    held(&pair, &["branch", "-m", "topic", "t9"], 128, renamed);
    for args in [&["branch", "-m", "t9"][..], &["branch", "-M", "t9"]] {
        let said = "fatal: cannot rename the current branch while not on any\n";
        of_late::same_failure(&pair, args, 128, said);
    }
    let deleted = "error: cannot delete branch 'topic' used by worktree at '";
    held(&pair, &["branch", "-d", "topic"], 1, deleted);
    no_second_tree(&pair);
    assert_eq!(branches(&pair.alt), before);
    // a branch that is not in it moves and takes a name as ever
    pair.both(&["branch", "-f", "other", "main"]);
    pair.both(&["branch", "-m", "other", "o2"]);
    // once the rebase is over the branch is free
    pair.both(&["rebase", "--abort"]);
    pair.both(&["branch", "-m", "topic", "t9"]);
    pair.both(&["branch", "-m", "t9", "topic"]);
    assert_eq!(heads(&pair.alt), heads(&pair.git));

    pair.both(&["bisect", "start"]);
    pair.both(&["bisect", "bad"]);
    pair.both(&["bisect", "good", "main~1"]);
    let before = ok(alt(&pair.alt, &["rev-parse", "main", "topic", "o2"]));
    held(&pair, &["branch", "-f", "topic", "main"], 128, forced);
    let renamed = "fatal: branch refs/heads/topic is being bisected at ";
    held(&pair, &["branch", "-m", "topic", "t9"], 128, renamed);
    held(&pair, &["branch", "-d", "topic"], 1, deleted);
    no_second_tree(&pair);
    assert_eq!(
        ok(alt(&pair.alt, &["rev-parse", "main", "topic", "o2"])),
        before
    );
    pair.both(&["bisect", "reset"]);
    pair.both(&["branch", "-m", "topic", "t9"]);
}

#[test]
fn a_branch_another_workspace_rebases_is_not_taken_from_it() {
    let pair = history();
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(&pair.alt, &["switch", "main"]));
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap()],
    ));
    ok(alt(&tree, &["switch", "topic"]));
    fails(alt(&tree, &["rebase", "main", "-x", "false"]), 1);
    let before = branches(&pair.alt);
    let at = format!(
        "used by worktree at '{}'\n",
        tree.canonicalize().unwrap().display()
    );
    for args in [
        &["switch", "topic"][..],
        &["switch", "-C", "topic", "main"],
        &["checkout", "topic"],
    ] {
        let said = fails(alt(&pair.alt, args), 128);
        assert!(
            said.starts_with(&format!("fatal: 'topic' is already {at}")),
            "{args:?}: {said}"
        );
    }
    let said = fails(alt(&pair.alt, &["branch", "-f", "topic", "main"]), 128);
    assert!(
        said.starts_with("fatal: cannot force update the branch 'topic'") && said.contains(&at)
    );
    let said = fails(alt(&pair.alt, &["branch", "-m", "topic", "t9"]), 128);
    let rebased = format!(
        "fatal: branch refs/heads/topic is being rebased at {}\n",
        tree.canonicalize().unwrap().display()
    );
    assert!(said.starts_with(&rebased), "{said}");
    let said = fails(alt(&pair.alt, &["branch", "-d", "topic"]), 1);
    assert!(said.starts_with("error: cannot delete branch 'topic'") && said.contains(&at));
    let third = trees.path().join("third");
    let args = [
        "workspace",
        "add",
        "third",
        third.to_str().unwrap(),
        "topic",
    ];
    let said = fails(alt(&pair.alt, &args), 128);
    assert!(
        said.starts_with(&format!("fatal: 'topic' is already {at}")),
        "{said}"
    );
    assert!(!third.join(".alt").exists());
    assert_eq!(branches(&pair.alt), before);
    // once the rebase is given up and the workspace elsewhere, the branch is free
    ok(alt(&tree, &["rebase", "--abort"]));
    ok(alt(&tree, &["switch", "-c", "elsewhere"]));
    ok(alt(&pair.alt, &["switch", "topic"]));
}
