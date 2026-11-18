//! An index that names a blob the store does not hold: the index is
//! written with its own fsync, so after a power cut it can be ahead of
//! the object store. The check reports the entry, a commit refuses to
//! build a tree on it, and adding the file again stores the blob.

use std::fs;
use std::path::Path;

use super::crash_fixture::remove_final_log_frame;
use super::unconfirmed::{alt, ok, stderr};
use super::unconfirmed_tails::{LOGS, MARKERS, lag, lens};

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Runs `stage` and then takes the object store back to where it was
/// before, markers and all: the objects `stage` stored never arrived.
/// The operation log and the index keep what `stage` wrote.
fn lose_objects_of(dir: &Path, stage: impl FnOnce()) {
    let before = lens(dir);
    let markers = MARKERS.map(|marker| fs::read(dir.join(".alt").join(marker)).unwrap());
    let log = fs::read(dir.join(".alt/oplog/durable")).unwrap();
    stage();
    for (file, len) in LOGS.iter().zip(before).take(4) {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(dir.join(".alt").join(file))
            .unwrap();
        file.set_len(len).unwrap();
    }
    let now = fs::read(dir.join(".alt/oplog/durable")).unwrap();
    lag(dir, &markers);
    // the operation log was synced: only the object store is behind
    assert_ne!(now, log);
    fs::write(dir.join(".alt/oplog/durable"), now).unwrap();
}

/// One commit, then `new.txt` staged and its blob lost.
fn staged_and_lost(dir: &Path) -> String {
    ok(alt(dir, &["init"]));
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    ok(alt(dir, &["add", "a.txt"]));
    ok(alt(dir, &["commit", "-m", "first"]));
    let head = ok(alt(dir, &["rev-parse", "HEAD"]));
    fs::write(dir.join("new.txt"), "never arrived\n").unwrap();
    lose_objects_of(dir, || drop(ok(alt(dir, &["add", "new.txt"]))));
    head
}

#[test]
fn a_commit_refuses_an_index_entry_whose_blob_is_not_stored() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let head = staged_and_lost(dir);
    let commit = alt(dir, &["commit", "-m", "second"]);
    assert!(!commit.status.success(), "{}", stdout(&commit));
    let said = stderr(&commit);
    assert!(
        said.contains("index entry new.txt names blob ")
            && said.contains(", which is not stored: add the file again"),
        "{said}"
    );
    assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])), head);
    // a file that was committed before is not looked up again
    ok(alt(dir, &["add", "new.txt"]));
    ok(alt(dir, &["commit", "-m", "second"]));
    assert_ne!(ok(alt(dir, &["rev-parse", "HEAD"])), head);
    ok(alt(dir, &["fsck"]));
}

#[test]
fn an_amended_commit_refuses_it_too() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let head = staged_and_lost(dir);
    let amend = alt(dir, &["commit", "--amend", "-m", "first, again"]);
    assert!(!amend.status.success());
    assert!(stderr(&amend).contains("index entry new.txt names blob "));
    assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])), head);
}

#[test]
fn the_check_reports_the_entries_of_every_workspace() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path().join("main");
    fs::create_dir(&dir).unwrap();
    let other = tmp.path().join("other");
    ok(alt(&dir, &["init"]));
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    ok(alt(&dir, &["add", "a.txt"]));
    ok(alt(&dir, &["commit", "-m", "first"]));
    ok(alt(&dir, &["branch", "feat"]));
    let tree = other.to_str().unwrap();
    ok(alt(&dir, &["workspace", "add", "other", tree, "feat"]));
    ok(alt(&dir, &["fsck"]));
    lose_objects_of(&dir, || {
        fs::write(dir.join("here.txt"), "in the first workspace\n").unwrap();
        ok(alt(&dir, &["add", "here.txt"]));
        fs::write(other.join("there.txt"), "in the second\n").unwrap();
        ok(alt(&other, &["add", "there.txt"]));
    });
    let check = alt(&dir, &["fsck"]);
    assert!(!check.status.success());
    let said = stderr(&check);
    assert!(said.contains("index entry here.txt names blob "), "{said}");
    assert!(
        said.contains("index entry there.txt of workspace other names blob "),
        "{said}"
    );
    assert!(!said.contains("a.txt"), "{said}");
}

/// An add that was interrupted before its completion was recorded, with
/// the blob it stored lost as well: recovery does not complete it.
#[test]
fn recovery_does_not_complete_an_add_whose_blob_is_not_stored() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    staged_and_lost(dir);
    remove_final_log_frame(dir);
    let pending: alt_json::Value =
        alt_json::from_str(&ok(alt(dir, &["operation", "pending", "--json"]))).unwrap();
    let id = pending["operations"][0]["operation_id"].as_str().unwrap();
    let log = fs::read(dir.join(".alt/oplog/log")).unwrap();
    let recover = alt(dir, &["operation", "recover", id]);
    assert!(!recover.status.success());
    let said = stderr(&recover);
    assert!(
        said.contains("recovery refused: index entry new.txt names blob "),
        "{said}"
    );
    assert_eq!(fs::read(dir.join(".alt/oplog/log")).unwrap(), log);
}
