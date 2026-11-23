//! `alt reset --soft` is refused while the index holds a conflict, as by
//! git, whatever left the conflict there.

use crate::like_git::{Pair, alt, fails, files, git, heads, index, ok};

/// `main` and `side` both rewrite `f`.
fn diverged() -> Pair {
    let pair = Pair::new();
    pair.write("f", "base\n");
    pair.commit_all("base");
    pair.both(&["switch", "-c", "side"]);
    pair.write("f", "side\n");
    pair.commit_all("side");
    pair.both(&["switch", "main"]);
    pair.write("f", "main\n");
    pair.commit_all("main");
    pair
}

fn assert_soft_reset_refused(pair: &Pair) {
    let before = heads(&pair.alt);
    let said = pair.same_failure(&["reset", "--soft", "HEAD~1"], 128);
    assert_eq!(
        said,
        "fatal: Cannot do a soft reset in the middle of a merge.\n"
    );
    assert_eq!(heads(&pair.alt), before);
}

#[test]
fn a_soft_reset_is_refused_where_a_pick_stopped_in_conflict() {
    let pair = diverged();
    let (ours, theirs) = pair.run(&["cherry-pick", "side"]);
    assert_eq!(
        (ours.status.code(), theirs.status.code()),
        (Some(1), Some(1))
    );
    assert_soft_reset_refused(&pair);
}

#[test]
fn a_soft_reset_is_refused_where_a_stash_came_back_in_conflict() {
    let pair = diverged();
    pair.write("f", "put away\n");
    pair.both(&["stash"]);
    pair.write("f", "main two\n");
    pair.commit_all("main two");
    fails(alt(&pair.alt, &["stash", "pop"]), 1);
    fails(git(&pair.git, &["stash", "pop"]), 1);
    assert_soft_reset_refused(&pair);
}

#[test]
fn a_hard_reset_is_undone_with_the_pick_it_gave_up() {
    let pair = diverged();
    let (ours, theirs) = pair.run(&["cherry-pick", "side"]);
    assert_eq!(
        (ours.status.code(), theirs.status.code()),
        (Some(1), Some(1))
    );
    let picking = || ok(alt(&pair.alt, &["status", "--json"])).contains("\"kind\":\"cherr");
    let state = || (files(&pair.alt), index(&pair.alt), heads(&pair.alt));
    let before = state();
    assert!(picking());

    pair.both(&["reset", "-q", "--hard"]);
    pair.assert_same_state();
    assert!(!picking());
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(state(), before);
    assert!(picking());
    // the pick goes on from where the reset found it
    std::fs::write(pair.alt.join("f"), "both\n").unwrap();
    ok(alt(&pair.alt, &["add", "f"]));
    ok(alt(&pair.alt, &["cherry-pick", "--continue"]));
    assert!(!picking());
}
