//! How far the log was read for operations a workspace left open is kept
//! beside its index (`recovery.checked`), and a command that writes reads
//! on from there. What is kept must never hide an operation that was left
//! open: one prepared after it, one that a cut of the log uncovered
//! before it, or one beneath a log that grew differently after a cut.

use std::fs;
use std::path::Path;

use super::crash_fixture::remove_final_log_frame;
use super::unconfirmed::{alt, ok, stderr};

const CHECKED: &str = ".alt/recovery.checked";

/// A repository with one commit and three files added one by one after
/// it, so that the last add began with its check kept at the end of the
/// add before it.
fn three_adds(root: &Path) {
    ok(alt(root, &["init", "."]));
    fs::write(root.join("f"), "base").unwrap();
    ok(alt(root, &["add", "f"]));
    ok(alt(root, &["commit", "-m", "base"]));
    for name in ["a", "b", "c"] {
        fs::write(root.join(name), name).unwrap();
        ok(alt(root, &["add", name]));
    }
    assert!(root.join(CHECKED).exists());
}

fn refused_as_open(root: &Path, case: &str) {
    fs::write(root.join("late"), "late").unwrap();
    let add = alt(root, &["add", "late"]);
    assert!(!add.status.success(), "{case}: the add went through");
    assert!(
        stderr(&add).contains("outcome unknown"),
        "{case}: {}",
        stderr(&add)
    );
}

/// The add left open is found where the index is one a later add put in
/// place, whose record the cut took: it is abandoned with the index
/// kept, and the command goes on.
fn abandoned_as_superseded(root: &Path, case: &str) {
    fs::write(root.join("late"), "late").unwrap();
    let add = alt(root, &["add", "late"]);
    let told = stderr(&add);
    assert!(
        told.contains("warning: abandoned operation "),
        "{case}: {told}"
    );
    assert!(
        told.contains("What the index stages now is kept"),
        "{case}: {told}"
    );
    ok(add);
}

#[test]
fn an_add_left_open_is_found_wherever_the_last_check_stopped() {
    // frames cut off the end of the log: the completion of the last add;
    // that add as well, and the completion of the one before, which is
    // where the last check was kept
    for cut in [1, 3] {
        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path();
        three_adds(root);
        let kept = fs::read(root.join(CHECKED)).unwrap();
        for _ in 0..cut {
            remove_final_log_frame(root);
        }
        let case = format!("{cut} frames cut");
        if cut == 3 {
            // the index is the one the last add left
            abandoned_as_superseded(root, &case);
            continue;
        }
        refused_as_open(root, &case);
        // a refusal keeps nothing new
        assert_eq!(fs::read(root.join(CHECKED)).unwrap(), kept);
    }
}

#[test]
fn a_log_that_grew_differently_after_a_cut_is_read_from_its_start() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path().join("repo");
    let peer = dir.path().join("peer");
    fs::create_dir(&root).unwrap();
    three_adds(&root);
    ok(alt(
        &root,
        &["workspace", "add", "peer", peer.to_str().unwrap()],
    ));
    for name in ["d", "e"] {
        fs::write(root.join(name), name).unwrap();
        ok(alt(&root, &["add", name]));
    }
    // the log ends with the add of `d` left open; the check of the last
    // add was kept at that add's completion, which is gone
    for _ in 0..3 {
        remove_final_log_frame(&root);
    }
    // another workspace's add puts other ops where the cut ones were
    fs::write(peer.join("p"), "p").unwrap();
    ok(alt(&peer, &["add", "p"]));
    // the index is the one the add of `e` left
    abandoned_as_superseded(&root, "regrown");
}

#[test]
fn a_file_that_is_not_a_kept_check_is_passed_over_and_written_anew() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    three_adds(root);
    let kept = fs::read(root.join(CHECKED)).unwrap();
    for damaged in [&b""[..], b"not a check", &kept[..kept.len() - 1]] {
        fs::write(root.join(CHECKED), damaged).unwrap();
        fs::write(root.join("g"), damaged).unwrap();
        ok(alt(root, &["add", "g"]));
        assert_eq!(fs::read(root.join(CHECKED)).unwrap().len(), kept.len());
    }
    remove_final_log_frame(root);
    refused_as_open(root, "after a damaged file");
}
