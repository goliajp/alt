//! The index that takes the place of a torn one after a command that
//! began an operation, ended it or was given up. What such a command
//! writes in the log beside its checkouts and ref transactions, the mark,
//! the guard and the record that closes it, is the index of no workspace;
//! and a checkout it was given up over left the index as it found it.

use std::path::Path;

use super::super::staged_durability::power_cut;
use super::{INDEX, NOT_WHOLE, Torn, tear, tree};
use crate::step_crash::bench::{Bench, alt, copy, run, said};
use crate::step_given_up::killed_at;

/// `main` and `side` part after one commit and both change `f.txt`, `main`
/// twice; `side` adds `new.txt`. A file that holds nothing is tracked: the
/// blob the index of a guard names is stored.
fn two_sides() -> Bench {
    let bench = Bench::new();
    let repo = bench.repo();
    run(&repo, &["init", "."], 0);
    bench.write("e.txt", "");
    bench.write("k.txt", "k\n");
    bench.write("f.txt", "1\n2\n3\n");
    bench.commit("files");
    run(&repo, &["switch", "-c", "side"], 0);
    bench.write("f.txt", "1\nside\n3\n");
    bench.write("new.txt", "new\n");
    bench.commit("side");
    run(&repo, &["switch", "main"], 0);
    bench.write("f.txt", "1\nmain\n3\n");
    bench.commit("main");
    bench.write("f.txt", "1\nmain again\n3\n");
    bench.commit("main again");
    bench
}

/// An add that is not synced, then a power cut that takes what it wrote
/// and leaves the index file full of zeros: the index every command goes
/// by, and the next that writes puts in place, is `index`.
fn torn_after_an_add(bench: &Bench, index: &str, when: &str) {
    let repo = bench.repo();
    bench.write("x.txt", "x\n");
    run(&repo, &["add", "x.txt"], 0);
    power_cut(&repo);
    tear(&repo.join(INDEX), Torn::Zeros);

    let status = alt(&repo, &["status"], None);
    let told = said(&status);
    assert!(status.status.success(), "{when}: {told}");
    assert!(told.contains(NOT_WHOLE), "{when}: {told}");
    for staged in ["deleted:", "new file:", "Unmerged", "modified:"] {
        assert!(!told.contains(staged), "{when}: {told}");
    }
    let added = alt(&repo, &["add", "x.txt"], None);
    let told = said(&added);
    assert!(added.status.success(), "{when}: {told}");
    assert!(told.contains("it was restored to"), "{when}: {told}");
    let found = bench.shape().index;
    let but_x: Vec<&str> = found.lines().filter(|l| !l.ends_with("\tx.txt")).collect();
    assert_eq!(but_x, index.lines().collect::<Vec<_>>(), "{when}");
}

/// `stop` stops on the conflict in `f.txt`, which is resolved, and `end`
/// ends the operation with a ref transaction and no checkout: the index
/// that takes the place of a torn one later is the tree of the commit.
fn ended(stop: &[&str], end: &[&str]) {
    let bench = two_sides();
    let repo = bench.repo();
    let when = format!("{stop:?} ended by {end:?}");
    run(&repo, stop, 1);
    bench.write("f.txt", "resolved\n");
    run(&repo, &["add", "f.txt"], 0);
    run(&repo, end, 0);
    let ended = bench.shape();
    assert!(ended.index.contains("\te.txt"), "{when}");

    torn_after_an_add(&bench, &ended.index, &when);
    run(&repo, &["commit", "-m", "x"], 0);
    let tree = tree(&repo);
    for file in ["e.txt", "f.txt", "k.txt", "x.txt"] {
        assert!(tree.contains(file), "{when}: {file} of {tree}");
    }
    assert!(alt(&repo, &["fsck"], None).status.success(), "{when}");
}

#[test]
fn the_guard_of_a_merge_a_commit_ended_is_not_the_index_a_torn_one_is_restored_to() {
    ended(&["merge", "side"], &["commit", "-m", "merged"]);
}

#[test]
fn the_guard_of_a_cherry_pick_a_commit_ended_is_not_the_index_a_torn_one_is_restored_to() {
    ended(&["cherry-pick", "side"], &["commit", "-m", "picked"]);
}

#[test]
fn the_guard_of_a_cherry_pick_gone_on_with_is_not_the_index_a_torn_one_is_restored_to() {
    ended(&["cherry-pick", "side"], &["cherry-pick", "--continue"]);
}

#[test]
fn the_guard_of_a_revert_a_commit_ended_is_not_the_index_a_torn_one_is_restored_to() {
    ended(&["revert", "HEAD~1"], &["commit", "-m", "reverted"]);
}

/// `command` is killed before it wrote the first file of its checkout and
/// given up: nothing is there to put back, and the checkout is closed and
/// not gone on with.
fn given_up_before_a_file(bench: &Bench, command: &[&str]) -> String {
    let repo = bench.repo();
    copy(&repo, &bench.kept("fixture"));
    let before = bench.shape();
    let (_, id) = killed_at(bench, command, "checkout-file:before");
    let given_up = run(&repo, &["operation", "abandon", &id], 0);
    assert!(given_up.contains("given up"), "{given_up}");
    assert_eq!(bench.shape(), before, "{command:?}");
    before.index
}

fn state_file(repo: &Path, name: &str) -> bool {
    repo.join(".alt").join(name).exists()
}

#[test]
fn a_merge_given_up_before_it_wrote_a_file_left_the_index_a_torn_one_is_restored_to() {
    let bench = two_sides();
    let index = given_up_before_a_file(&bench, &["merge", "side"]);
    assert!(!state_file(&bench.repo(), "MERGE_HEAD"));
    torn_after_an_add(&bench, &index, "a merge given up");
    assert!(!state_file(&bench.repo(), "MERGE_HEAD"));
}

#[test]
fn a_rebase_given_up_by_moving_its_refs_back_left_the_index_a_torn_one_is_restored_to() {
    // the rebase begins with a ref transaction that carries its checkout,
    // and giving it up moves the refs back and has no file to put back
    let bench = two_sides();
    let repo = bench.repo();
    run(&repo, &["switch", "side"], 0);
    let head = run(&repo, &["rev-parse", "HEAD"], 0);
    let index = given_up_before_a_file(&bench, &["rebase", "main"]);
    assert!(!state_file(&repo, "REBASE_STATE"));
    torn_after_an_add(&bench, &index, "a rebase given up");
    assert_eq!(run(&repo, &["rev-parse", "HEAD"], 0), head);
}
