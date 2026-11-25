//! A rebase forgotten leaves the index as the rebase had it, on a branch
//! that did not move: where the two differ a commit made next would take
//! out what the rebase had not replayed, which is said with the way back.

use crate::like_git::{Pair, alt, fails, ok};

const BACK: &str = "`alt reset` puts the index back to topic";

/// `topic` two commits past `base`, with `main` one past it.
fn topic() -> Pair {
    let pair = Pair::new();
    pair.write("f", "base\n");
    pair.write("other", "base\n");
    pair.commit_all("base");
    pair.both(&["switch", "-c", "topic"]);
    pair.write("f", "t1\n");
    pair.commit_all("t1");
    pair.write("other", "t2\n");
    pair.commit_all("t2");
    pair.both(&["switch", "main"]);
    pair.write("m", "m\n");
    pair.commit_all("m");
    pair.both(&["switch", "topic"]);
    pair
}

#[test]
fn a_rebase_forgotten_midway_says_the_index_is_not_the_branch_and_how_to_put_it_back() {
    let pair = topic();
    let tip = ok(alt(&pair.alt, &["rev-parse", "topic"]));
    fails(alt(&pair.alt, &["rebase", "-x", "test -f ok", "main"]), 1);
    let told = ok(alt(&pair.alt, &["rebase", "--quit"]));
    assert!(told.contains(BACK), "{told}");
    assert_eq!(ok(alt(&pair.alt, &["rev-parse", "HEAD"])), tip);
    // the index has `m` and has not what `t2` made of `other`
    let status = ok(alt(&pair.alt, &["status"]));
    assert!(status.contains("other") && status.contains("m"), "{status}");
    ok(alt(&pair.alt, &["reset", "--hard"]));
    let status = ok(alt(&pair.alt, &["status"]));
    assert!(status.contains("nothing to commit"), "{status}");
    assert_eq!(std::fs::read(pair.alt.join("other")).unwrap(), b"t2\n");
    assert_eq!(ok(alt(&pair.alt, &["rev-parse", "HEAD"])), tip);
}

#[test]
fn a_rebase_forgotten_with_the_index_of_its_branch_says_nothing_of_the_index() {
    // one commit, replayed onto the commit it was made on: stopped
    // after it, the index is the branch
    let pair = Pair::new();
    pair.write("f", "base\n");
    pair.commit_all("base");
    pair.both(&["switch", "-c", "topic"]);
    pair.write("f", "t1\n");
    pair.commit_all("t1");
    fails(alt(&pair.alt, &["rebase", "-x", "test -f ok", "main"]), 1);
    let told = ok(alt(&pair.alt, &["rebase", "--quit"]));
    assert!(told.contains("The rebase is forgotten"), "{told}");
    assert!(!told.contains(BACK), "{told}");
}
