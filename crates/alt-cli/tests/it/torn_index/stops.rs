//! The note of a stop names the last operation of the log, and the log
//! is synced up to it when the note is written: operations that adds
//! left unsynced before the stop do not take the note with them when a
//! power cut takes them.

use std::fs;
use std::path::Path;

use super::super::staged_durability::{power_cut, repo};
use super::super::unconfirmed::{alt, ok, stderr};
use super::{INDEX, Torn, VERIFIED, tear, tree};

/// Two sides that both change `a.txt`, the other one adding `t.txt`;
/// `main` is checked out.
fn two_sides(dir: &Path) {
    repo(dir);
    ok(alt(dir, &["branch", "feat"]));
    fs::write(dir.join("a.txt"), "ours\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "ours"]));
    ok(alt(dir, &["switch", "feat"]));
    fs::write(dir.join("a.txt"), "theirs\n").unwrap();
    fs::write(dir.join("t.txt"), "theirs\n").unwrap();
    ok(alt(dir, &["add", "a.txt", "t.txt"]));
    ok(alt(dir, &["commit", "-m", "theirs"]));
    ok(alt(dir, &["switch", "main"]));
}

/// An add of a change and one that puts the file back: the index is what
/// it was, and the log ends in operations that no command synced.
fn adds_left_unsynced(dir: &Path) {
    fs::write(dir.join("a.txt"), "passing\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    fs::write(dir.join("a.txt"), "ours\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
}

/// After `stop` stopped on the conflict in `a.txt`, an add and a power
/// cut that tears the index: the index the stop left is put back.
fn stop_outlives_the_cut(stop: &[&str], go_on: &[&str]) {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    two_sides(dir);
    adds_left_unsynced(dir);
    assert!(!alt(dir, stop).status.success());
    fs::write(dir.join("a.txt"), "both\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    power_cut(dir);
    tear(&dir.join(INDEX), Torn::Zeros);

    let status = alt(dir, &["status"]);
    let told = stderr(&status);
    assert!(
        told.contains("the index the merge that stopped left"),
        "{stop:?}: {told}"
    );
    assert!(!told.contains("is lost"), "{stop:?}: {told}");
    assert!(ok(status).contains("Unmerged paths:"), "{stop:?}");
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, go_on));
    // what the other side added and nobody staged again is in the commit
    let tree = tree(dir);
    assert!(tree.contains("t.txt"), "{stop:?}: {tree}");
    assert!(ok(alt(dir, &["fsck"])).ends_with(VERIFIED));
}

#[test]
fn a_merge_that_stopped_after_adds_left_unsynced_is_put_back_to_its_stop() {
    stop_outlives_the_cut(&["merge", "feat"], &["commit", "-m", "merged"]);
}

#[test]
fn a_cherry_pick_that_stopped_after_adds_left_unsynced_is_put_back_to_its_stop() {
    stop_outlives_the_cut(&["cherry-pick", "feat"], &["commit", "-m", "picked"]);
}
