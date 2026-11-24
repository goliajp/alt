//! `alt commit --amend` that would leave a commit changing nothing is
//! refused as by git, unless `--allow-empty` asks for it.

use crate::like_git::{Pair, alt, alt_daemon, fails, git, ok};

const SAID: &str = "You asked to amend the most recent commit, but doing so would make\n\
                    it empty. You can repeat your command with --allow-empty, or you can\n\
                    remove the commit entirely with \"git reset HEAD^\".\n";

/// The amend is refused by both in git's words, and the commit stays.
fn refused(pair: &Pair, args: &[&str]) {
    let at = ok(alt(&pair.alt, &["rev-parse", "HEAD"]));
    let ours = fails(alt(&pair.alt, args), 1);
    assert!(ours.starts_with(SAID), "{args:?}: {ours}");
    let theirs = fails(git(&pair.git, args), 1);
    assert!(theirs.starts_with(SAID), "{args:?}: {theirs}");
    assert_eq!(ok(alt(&pair.alt, &["rev-parse", "HEAD"])), at);
}

fn same_head(pair: &Pair) {
    let ours = ok(alt(&pair.alt, &["rev-parse", "HEAD"]));
    assert_eq!(ours, ok(git(&pair.git, &["rev-parse", "HEAD"])));
}

#[test]
fn an_amend_that_leaves_nothing_of_the_commit_is_refused() {
    let pair = Pair::new();
    pair.write("a", "a\n");
    pair.commit_all("root");
    // the first commit with nothing left in it
    pair.both(&["rm", "-q", "--cached", "a"]);
    refused(&pair, &["commit", "--amend", "-m", "nothing"]);
    pair.both(&["add", "a"]);

    pair.write("b", "b\n");
    pair.commit_all("second");
    pair.both(&["rm", "-q", "--cached", "b"]);
    refused(&pair, &["commit", "--amend", "-m", "nothing"]);
    refused(&pair, &["commit", "--amend", "--no-edit"]);
    let served = fails(
        alt_daemon(&pair.alt, &["commit", "--amend", "--no-edit"]),
        1,
    );
    assert!(served.starts_with(SAID), "{served}");
    // asked for, it is made, and then only its message is no reason either
    pair.both(&["commit", "--amend", "--allow-empty", "-m", "empty"]);
    same_head(&pair);
    refused(&pair, &["commit", "--amend", "-m", "empty still"]);
    pair.both(&["commit", "--amend", "--allow-empty", "-m", "empty still"]);
    same_head(&pair);
    // with a change of its own again it needs no asking
    pair.both(&["add", "b"]);
    pair.both(&["commit", "--amend", "-m", "second again"]);
    same_head(&pair);
}

#[test]
fn a_merge_amended_to_the_tree_of_a_parent_is_made() {
    let pair = Pair::new();
    pair.write("f", "0\n");
    pair.commit_all("base");
    pair.both(&["branch", "side"]);
    pair.write("f", "ours\n");
    pair.commit_all("ours");
    pair.both(&["switch", "side"]);
    pair.write("g", "theirs\n");
    pair.commit_all("theirs");
    pair.both(&["switch", "main"]);
    pair.merge_at("side", 1_700_000_100);
    pair.both(&["rm", "-q", "--cached", "g"]);
    pair.both(&["commit", "--amend", "-m", "merged, with nothing of side"]);
    same_head(&pair);
}
