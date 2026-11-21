//! Which index takes the place of a torn one: the latest the operation
//! log vouches for, never one from before the last commit, and for a
//! merge that stopped the index it left.

use std::fs;
use std::path::Path;

use super::super::staged_durability::{add, power_cut, repo};
use super::super::sync_order::{FILES, objects_synced};
use super::super::unconfirmed::{alt, ok, stderr};
use super::{INDEX, NOT_WHOLE, Run, Torn, VERIFIED, parents, served, tear, tree};

#[test]
fn what_an_add_staged_stays_staged_where_all_but_the_index_file_outlived_the_cut() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    add(dir, "b.txt");
    tear(&dir.join(INDEX), Torn::Zeros);
    let status = ok(alt(dir, &["status"]));
    assert!(status.contains("new file:   b.txt"), "{status}");
    let committed = alt(dir, &["commit", "-m", "second"]);
    let told = stderr(&committed);
    assert!(
        told.contains("it was restored to the index operation"),
        "{told}"
    );
    ok(committed);
    // the commit put no index in place of its own, and the file is whole
    let status = alt(dir, &["status"]);
    assert!(!stderr(&status).contains(NOT_WHOLE));
    let tree = tree(dir);
    assert!(tree.contains("a.txt") && tree.contains("b.txt"), "{tree}");
    assert!(ok(alt(dir, &["fsck"])).ends_with(VERIFIED));
}

/// One synced commit, `b.txt` added and not synced, then a power cut that
/// took the blob of the add and left its record in the log, its snapshot
/// and, with `named`, the record that names the blob: no command takes
/// the add for staged.
fn blob_lost(named: bool, run: Run) {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    add(dir, "b.txt");
    let alt_dir = dir.join(".alt");
    for (name, len) in FILES.iter().zip(objects_synced(&alt_dir)) {
        if named && *name == "map.alt" {
            continue;
        }
        let file = fs::OpenOptions::new().write(true).open(alt_dir.join(name));
        file.unwrap().set_len(len).unwrap();
    }
    super::super::power_cut::another_boot(&alt_dir);
    tear(&dir.join(INDEX), Torn::Empty);
    let status = ok(run(dir, &["status"]));
    assert!(!status.contains("new file"), "named {named}: {status}");
    assert!(status.contains("Untracked files:") && status.contains("b.txt"));
    ok(run(dir, &["add", "b.txt"]));
    ok(run(dir, &["commit", "-m", "second"]));
    assert!(ok(alt(dir, &["fsck"])).ends_with(VERIFIED));
}

#[test]
fn an_index_that_names_a_blob_the_power_cut_took_does_not_take_the_place_of_a_torn_one() {
    blob_lost(false, alt);
}

#[test]
fn a_daemon_takes_no_index_that_names_a_blob_the_power_cut_took_for_a_torn_one() {
    blob_lost(false, served);
}

#[test]
fn an_index_is_not_taken_for_a_torn_one_where_only_the_name_of_its_blob_outlived_the_cut() {
    blob_lost(true, alt);
    blob_lost(true, served);
}

fn snapshots(dir: &Path) -> Vec<std::path::PathBuf> {
    let listing = fs::read_dir(dir.join(".alt/recovery/objects")).unwrap();
    listing.map(|entry| entry.unwrap().path()).collect()
}

#[test]
fn a_torn_index_is_not_put_back_to_one_from_before_the_last_commit() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let before = snapshots(dir);
    add(dir, "b.txt");
    ok(alt(dir, &["commit", "-m", "second"]));
    add(dir, "c.txt");
    power_cut(dir);
    // the index the commit was made from has no snapshot left: an older
    // build's commit would not have synced it
    for snapshot in snapshots(dir) {
        if !before.contains(&snapshot) {
            fs::remove_file(snapshot).unwrap();
        }
    }
    tear(&dir.join(INDEX), Torn::Empty);
    let status = alt(dir, &["status"]);
    let told = stderr(&status);
    assert!(
        told.contains("this shows the workspace with the tree of HEAD"),
        "{told}"
    );
    // with the index from before the commit, b.txt would show as deleted
    let shown = ok(status);
    assert!(
        !shown.contains("b.txt") && shown.contains("c.txt"),
        "{shown}"
    );
    let added = alt(dir, &["add", "c.txt"]);
    let told = stderr(&added);
    assert!(
        told.contains("it was restored to the tree of HEAD"),
        "{told}"
    );
    ok(added);
    ok(alt(dir, &["commit", "-m", "third"]));
    let tree = tree(dir);
    assert!(tree.contains("b.txt") && tree.contains("c.txt"), "{tree}");
    assert!(ok(alt(dir, &["fsck"])).ends_with(VERIFIED));
}

#[test]
fn the_index_a_stopped_merge_left_takes_the_place_of_a_torn_one() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
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
    assert!(!alt(dir, &["merge", "feat"]).status.success());
    fs::write(dir.join("a.txt"), "both\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    power_cut(dir);
    tear(&dir.join(INDEX), Torn::Zeros);

    let status = alt(dir, &["status"]);
    let told = stderr(&status);
    assert!(
        told.contains("the index the merge that stopped left"),
        "{told}"
    );
    assert!(ok(status).contains("Unmerged paths:"));
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "merged"]));
    assert_eq!(parents(dir).len(), 2);
    // what the other side added and nobody staged again is in the merge
    let tree = tree(dir);
    assert!(tree.contains("t.txt"), "{tree}");
    assert!(ok(alt(dir, &["fsck"])).ends_with(VERIFIED));
}

#[test]
fn a_torn_index_of_one_workspace_is_restored_there_and_no_other_is_told() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = &tmp.path().join("repo");
    fs::create_dir(dir).unwrap();
    repo(dir);
    ok(alt(dir, &["branch", "feat"]));
    let side = &tmp.path().join("side");
    let at = side.to_str().unwrap();
    ok(alt(dir, &["workspace", "add", "side", at, "feat"]));
    add(side, "b.txt");
    power_cut(dir);
    tear(&dir.join(".alt/workspaces/side/index"), Torn::InHeader);

    let status = alt(dir, &["status"]);
    assert!(!stderr(&status).contains(NOT_WHOLE));
    ok(status);
    let check = ok(alt(dir, &["fsck"]));
    let named = "the index file of workspace side is not a whole index";
    assert!(check.contains(named), "{check}");
    let status = alt(side, &["status"]);
    assert!(stderr(&status).contains(NOT_WHOLE));
    assert!(ok(status).contains("b.txt"));
    let added = alt(side, &["add", "b.txt"]);
    assert!(stderr(&added).contains("it was restored to"));
    ok(added);
    ok(alt(side, &["commit", "-m", "on feat"]));
    assert!(ok(alt(dir, &["fsck"])).ends_with(VERIFIED));
}

#[test]
fn an_add_cut_short_whose_index_is_torn_as_well_is_abandoned_over_the_restored_index() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    super::super::abandoned_add::add_cut_short(dir, true);
    tear(&dir.join(INDEX), Torn::NoChecksum);
    let added = alt(dir, &["add", "new.txt"]);
    let told = stderr(&added);
    assert!(
        told.contains("it was restored to the index operation"),
        "{told}"
    );
    assert!(told.contains("warning: abandoned operation"), "{told}");
    ok(added);
    ok(alt(dir, &["commit", "-m", "second"]));
    assert!(ok(alt(dir, &["fsck"])).ends_with(VERIFIED));
}
