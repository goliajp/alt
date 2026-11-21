//! A command that begins an operation, ends it or takes one of those
//! back leaves the operation log recorded as synced to its end. A command
//! that reads looks through what lies past that record for ops that were
//! never synced, from the start of the log where the refs it has kept are
//! of its end: with nothing past the record, it reads none of the log
//! twice and none of it for that.

use std::path::Path;

use crate::capability_gate::{alt, fixture, ok};
use crate::sync_order::{log_len, log_synced};

fn synced(repo: &Path, when: &str) {
    let alt_dir = repo.join(".alt");
    assert_eq!(log_synced(&alt_dir), log_len(&alt_dir), "after {when}");
}

#[test]
fn the_log_is_recorded_as_synced_to_its_end_after_each_step() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let repo = fixture(&tmp);
    let commit = |text: &str, message: &str| {
        std::fs::write(repo.join("a.txt"), text).unwrap();
        ok(alt(repo, &["add", "a.txt"]));
        ok(alt(repo, &["commit", "-m", message]));
    };
    ok(alt(repo, &["switch", "-c", "feature/side"]));
    commit("side\n", "side");
    ok(alt(repo, &["switch", "main"]));
    commit("main\n", "main");
    synced(repo, "a commit");

    assert_eq!(alt(repo, &["merge", "feature/side"]).status.code(), Some(1));
    synced(repo, "a merge that stops");
    ok(alt(repo, &["status"]));
    synced(repo, "a status");
    ok(alt(repo, &["merge", "--abort"]));
    synced(repo, "the abort");
    ok(alt(repo, &["undo"]));
    synced(repo, "the undo of the abort");
    std::fs::write(repo.join("a.txt"), "both\n").unwrap();
    ok(alt(repo, &["add", "a.txt"]));
    ok(alt(repo, &["commit", "-m", "merged"]));
    synced(repo, "the commit that concludes the merge");

    assert_eq!(
        alt(repo, &["revert", "HEAD~1", "HEAD"]).status.code(),
        Some(1)
    );
    synced(repo, "a revert that stops");
    ok(alt(repo, &["revert", "--abort"]));
    synced(repo, "its abort");
}
