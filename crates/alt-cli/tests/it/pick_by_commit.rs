//! A cherry-pick that stopped, concluded by `alt commit`: the commit keeps
//! the author of the commit picked and when it was written, as git's does.

use crate::cherry_pick_resume::{by_another_author, header, shown, stop};
use crate::scan_daemon::output;
use crate::stopped_trees::{TREES, alike, commit, every, operation, succeeded};

#[test]
fn a_commit_that_concludes_a_pick_keeps_its_author_as_in_git() {
    let trees = by_another_author();
    stop(&trees);
    trees.write("f.txt", "1\nresolved\n3\n");
    commit(&trees, "picked by hand");
    alike(&trees, "after the commit");
    assert_eq!(operation(&trees), "null");
    for tree in TREES {
        let (commit, picked) = (shown(&trees, tree, "HEAD"), shown(&trees, tree, "side"));
        assert_eq!(header(&commit, "author "), header(&picked, "author "));
        assert_eq!(header(&commit, "author ").0, "author another <a@e");
        assert_eq!(header(&commit, "committer ").0, "committer tester <t@e");
        assert!(commit.ends_with("\n\npicked by hand\n"), "{tree}: {commit}");
    }

    // the next commit is no pick, and is its committer's own
    trees.write("u.txt", "after the pick\n");
    commit(&trees, "after");
    for tree in TREES {
        let commit = shown(&trees, tree, "HEAD");
        assert_eq!(header(&commit, "author ").0, "author tester <t@e", "{tree}");
    }
}

#[test]
fn a_merge_is_refused_while_a_pick_waits_for_its_commit_as_in_git() {
    let trees = by_another_author();
    stop(&trees);
    // resolved to what the branch has: nothing is staged, and the pick waits
    trees.write("f.txt", "1\nmain\n3\n");
    assert_eq!(every(&trees, &["add", "f.txt"]), [Some(0); 3]);
    assert_eq!(succeeded(&trees, &["merge", "side"]), [false; 3]);
    alike(&trees, "after the refusal");
    assert_eq!(operation(&trees), r#"{"kind":"cherry-pick"}"#);
    let said = output(&trees.at("direct"), &["merge", "side"], false);
    let said = String::from_utf8_lossy(&said.stderr).into_owned();
    assert!(
        said.contains("merge: a cherry-pick is in progress"),
        "{said}"
    );
}

#[test]
fn a_commit_that_concludes_another_operation_is_not_the_picks() {
    let trees = by_another_author();
    stop(&trees);
    let at = trees.at("direct");
    std::fs::write(at.join("f.txt"), "1\nmain\n3\n").unwrap();
    assert!(output(&at, &["add", "f.txt"], false).status.success());
    // a revert that stops while the pick still waits: f.txt goes with the
    // commit that added it and was changed since
    let stopped = output(&at, &["revert", "HEAD~1"], false);
    assert_eq!(stopped.status.code(), Some(1));
    assert!(at.join(".alt/CHERRY_PICK_HEAD").exists());
    std::fs::write(at.join("f.txt"), "kept\n").unwrap();
    assert!(output(&at, &["add", "."], false).status.success());
    let done = output(&at, &["commit", "-m", "reverted by hand"], false);
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    let commit = shown(&trees, "direct", "HEAD");
    assert_eq!(header(&commit, "author ").0, "author tester <t@e");
}
