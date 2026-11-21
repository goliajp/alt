//! What an add left unsynced is in the cache of the kernel, and gone
//! from it where the file system went away under the machine while it
//! stayed up: a disk pulled and put back, a write-back that failed. A
//! command that moves a ref looks at what it is about to record as
//! synced first: a blob that is not there is not committed.

use std::fs;
use std::path::Path;

use super::staged_durability::repo;
use super::sync_order::{FILES, objects_synced};
use super::unconfirmed::{alt, ok, stderr};

/// The pack loses what was appended to it and not synced; the boot is
/// the one the appends were left in.
fn lose_unsynced_content(dir: &Path) {
    let alt_dir = dir.join(".alt");
    let synced = objects_synced(&alt_dir)[0];
    let pack = fs::OpenOptions::new()
        .write(true)
        .open(alt_dir.join(FILES[0]));
    let pack = pack.unwrap();
    assert!(pack.metadata().unwrap().len() > synced);
    pack.set_len(synced).unwrap();
}

fn write(dir: &Path, text: &str) {
    fs::write(dir.join("f.txt"), text).unwrap();
}

fn commit(dir: &Path, text: &str) {
    write(dir, text);
    ok(alt(dir, &["add", "f.txt"]));
    ok(alt(dir, &["commit", "-m", text.trim()]));
}

/// `command` is refused for the blob of `f.txt`, moves no ref, and goes
/// through once the file is added again.
fn refused_until_added_again(dir: &Path, command: &[&str]) {
    let head = ok(alt(dir, &["rev-parse", "HEAD"]));
    let out = alt(dir, command);
    let said = stderr(&out);
    assert!(!out.status.success(), "{command:?}: {said}");
    assert!(
        said.contains("index entry f.txt names blob ")
            && said.contains(", which is not stored: add the file again"),
        "{command:?}: {said}"
    );
    assert_eq!(ok(alt(dir, &["rev-parse", "HEAD"])), head, "{command:?}");
    ok(alt(dir, &["add", "f.txt"]));
    ok(alt(dir, command));
    assert_ne!(ok(alt(dir, &["rev-parse", "HEAD"])), head, "{command:?}");
    let check = alt(dir, &["fsck"]);
    assert!(stderr(&check).is_empty(), "{}", stderr(&check));
    assert!(ok(check).ends_with("every record verified\n"));
    let stored = ok(alt(dir, &["show", "HEAD:f.txt"]));
    assert_eq!(stored, fs::read_to_string(dir.join("f.txt")).unwrap());
}

#[test]
fn a_commit_without_its_checks_does_not_take_a_blob_the_running_boot_lost() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    write(dir, "staged and lost\n");
    ok(alt(dir, &["add", "f.txt"]));
    lose_unsynced_content(dir);
    refused_until_added_again(dir, &["commit", "--no-verify", "-m", "second"]);
}

#[test]
fn a_revert_does_not_go_on_with_a_blob_the_running_boot_lost() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    commit(dir, "one\n");
    commit(dir, "two\n");
    commit(dir, "three\n");
    let stopped = alt(dir, &["revert", "HEAD~1"]);
    assert!(!stopped.status.success());
    write(dir, "settled\n");
    ok(alt(dir, &["add", "f.txt"]));
    lose_unsynced_content(dir);
    refused_until_added_again(dir, &["revert", "--continue"]);
}

#[test]
fn a_rebase_does_not_go_on_with_a_blob_the_running_boot_lost() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    commit(dir, "one\n");
    ok(alt(dir, &["branch", "topic"]));
    commit(dir, "two\n");
    let onto = ok(alt(dir, &["rev-parse", "HEAD"]));
    ok(alt(dir, &["switch", "topic"]));
    commit(dir, "topic\n");
    let stopped = alt(dir, &["rebase", onto.trim()]);
    assert!(!stopped.status.success());
    write(dir, "settled\n");
    ok(alt(dir, &["add", "f.txt"]));
    lose_unsynced_content(dir);
    refused_until_added_again(dir, &["rebase", "--continue"]);
}

/// A command that only moves a ref records the operations of the adds
/// before it as synced, and with them what they stored: it looks first.
#[test]
fn a_branch_made_before_the_commit_does_not_record_the_lost_blob_as_synced() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    write(dir, "staged and lost\n");
    ok(alt(dir, &["add", "f.txt"]));
    lose_unsynced_content(dir);
    ok(alt(dir, &["branch", "x"]));
    refused_until_added_again(dir, &["commit", "--no-verify", "-m", "second"]);
}
