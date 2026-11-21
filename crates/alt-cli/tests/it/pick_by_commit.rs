//! A cherry-pick that stopped, concluded by `alt commit`: the commit keeps
//! the author of the commit picked and when it was written, as git's does.

use crate::cherry_pick_resume::{by_another_author, header, shown, stop};
use crate::stopped_trees::{TREES, alike, commit, operation};

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
