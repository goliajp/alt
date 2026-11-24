//! An export made while an operation is under way is a repository git
//! opens: where a rebase or a bisect has HEAD off its branch, HEAD is the
//! commit they stand on, and an import of it leaves them standing.

mod beneath;

use std::path::Path;

use crate::like_git::{Pair, alt, export, fails, git, ok};

/// `main` gone one commit on from where `topic`, with two commits of its
/// own, left it. `topic` checked out.
fn history() -> Pair {
    let pair = Pair::new();
    pair.write("f", "1\n");
    pair.commit_all("base");
    pair.both(&["branch", "topic"]);
    pair.write("g", "2\n");
    pair.commit_all("main moves on");
    pair.both(&["switch", "topic"]);
    pair.write("a", "a\n");
    pair.commit_all("topic one");
    pair.write("b", "b\n");
    pair.commit_all("topic two");
    pair
}

/// Git opens the export of `repo`, sound, with HEAD at the commit HEAD is
/// at there and the refs under `refs/` and no other; the branch HEAD is
/// on, none where it is on none.
fn opened(repo: &Path) -> Option<String> {
    let out = export(repo);
    let exported = out.path().join("exported");
    ok(git(&exported, &["fsck", "--strict"]));
    ok(git(&exported, &["status"]));
    let at = ok(git(&exported, &["rev-parse", "HEAD"]));
    assert_eq!(at, ok(alt(repo, &["rev-parse", "HEAD"])));
    let listed = ok(git(&exported, &["for-each-ref", "--format=%(refname)"]));
    assert!(
        listed.lines().all(|name| name.starts_with("refs/")),
        "{listed}"
    );
    for apart in ["rebase", "bisect"] {
        assert!(!exported.join(".git").join(apart).exists(), "{apart}");
    }
    let on = git(&exported, &["symbolic-ref", "-q", "HEAD"]);
    on.status.success().then(|| ok(on))
}

#[test]
fn a_rebase_under_way_is_exported_as_the_commit_it_stands_on_and_goes_on_after_an_import() {
    let pair = history();
    let (ours, theirs) = pair.run(&["rebase", "main", "-x", "false"]);
    fails(ours, 1);
    fails(theirs, 1);
    assert_eq!(opened(&pair.alt), None);
    let topic = ok(alt(&pair.alt, &["rev-parse", "topic"]));

    // an import of what was exported finds the rebase where it was
    let out = export(&pair.alt);
    let exported = out.path().join("exported");
    let at = ok(alt(&pair.alt, &["rev-parse", "HEAD"]));
    ok(alt(&exported, &["import", pair.alt.to_str().unwrap()]));
    assert_eq!(ok(alt(&pair.alt, &["rev-parse", "HEAD"])), at);
    assert_eq!(ok(alt(&pair.alt, &["rev-parse", "topic"])), topic);
    assert!(ok(alt(&pair.alt, &["status"])).contains("rebase"));
    // the command fails after the second commit too, and then it is over
    let (ours, theirs) = pair.run(&["rebase", "--continue"]);
    fails(ours, 1);
    fails(theirs, 1);
    pair.both(&["rebase", "--continue"]);
    pair.assert_same_state();
    assert!(!ok(alt(&pair.alt, &["status"])).contains("rebase"));
}

#[test]
fn a_bisect_under_way_is_exported_as_the_commit_it_stands_on() {
    let pair = history();
    pair.both(&["bisect", "start"]);
    pair.both(&["bisect", "bad"]);
    pair.both(&["bisect", "good", "main~1"]);
    assert_eq!(opened(&pair.alt), None);
    let at = ok(git(&pair.git, &["rev-parse", "HEAD"]));
    assert_eq!(ok(alt(&pair.alt, &["rev-parse", "HEAD"])), at);
    pair.both(&["bisect", "reset"]);
    assert_eq!(opened(&pair.alt).as_deref(), Some("refs/heads/topic\n"));
}

#[test]
fn a_merge_a_pick_and_a_revert_under_way_are_exported_on_their_branch() {
    let pair = history();
    pair.both(&["switch", "main"]);
    pair.write("a", "main's\n");
    pair.commit_all("main has a too");
    let on_main = Some("refs/heads/main\n".to_owned());
    for (begin, end) in [
        (&["merge", "topic"][..], &["merge", "--abort"][..]),
        (&["cherry-pick", "topic~1"], &["cherry-pick", "--abort"]),
    ] {
        let (ours, theirs) = pair.run(begin);
        fails(ours, 1);
        fails(theirs, 1);
        assert_eq!(opened(&pair.alt), on_main, "{begin:?}");
        pair.both(end);
    }
    pair.write("a", "changed since\n");
    pair.commit_all("a changes again");
    let (ours, theirs) = pair.run(&["revert", "--no-edit", "HEAD~1"]);
    fails(ours, 1);
    fails(theirs, 1);
    assert_eq!(opened(&pair.alt), on_main);
    pair.both(&["revert", "--abort"]);
}

#[test]
fn a_rebase_in_another_workspace_leaves_the_export_on_the_branch_of_this_one() {
    let pair = history();
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(&pair.alt, &["switch", "main"]));
    let added = ["workspace", "add", "ws", tree.to_str().unwrap(), "topic"];
    ok(alt(&pair.alt, &added));
    fails(alt(&tree, &["rebase", "main", "-x", "false"]), 1);
    assert_eq!(opened(&pair.alt).as_deref(), Some("refs/heads/main\n"));
    ok(alt(&tree, &["rebase", "--abort"]));
}
