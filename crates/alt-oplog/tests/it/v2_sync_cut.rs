//! No sync of a version 2 log says of an op that was cut that it is
//! synced: not the handle's own, whatever the note beside the log says of
//! the log since, and not its sink's, whatever the handle did meanwhile.

use std::fs;
use std::path::Path;

use alt_oplog::{Op, OpLog, OpLogError};

use super::v2_image::linked;
use super::v2_log::{boot, grow, new_log};
use super::v2_named::collecting;

fn all(_: &Op) -> Result<bool, OpLogError> {
    Ok(true)
}

fn but_mine(op: &Op) -> Result<bool, OpLogError> {
    Ok(op.payload != b"mine")
}

/// A handle of boot 1 on a log in `dir` of two synced ops and an op
/// `mine` it appended and did not sync, which another handle then cut.
/// With `grown`, that one appended more than it cut and synced it: the
/// note beside the log says the log is synced past where `mine` ended.
fn held_past_mine(dir: &Path, deferred: bool, grown: bool) -> OpLog {
    boot(1);
    let mut held = new_log(dir);
    grow(&mut held, "a", 2);
    held.set_defer_durability(deferred);
    held.append("a", 3, b"mine").unwrap();
    let end = held.appended_len();
    // without a note, a handle looks at what is past the boundary
    fs::remove_file(dir.join("tail")).unwrap();
    let mut other = OpLog::open(dir).unwrap();
    assert_eq!(other.settle(but_mine).unwrap().unwrap().ops, 1);
    if grown {
        other.append("b", 4, &[7; 900]).unwrap();
        other.sync().unwrap();
        assert!(alt_oplog::bounded_in(dir).unwrap() >= end);
    }
    held
}

#[test]
fn a_sync_tells_of_the_cut_of_an_op_though_the_log_is_noted_as_synced_past_where_the_op_ended() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut held = held_past_mine(dir.path(), deferred, true);
        let sink = held.sync_handle().unwrap();
        let mut synced = held.sync();
        if deferred && synced.is_ok() {
            synced = sink.fsync();
        }
        assert!(
            matches!(synced, Err(OpLogError::CutBack)),
            "deferred: {deferred}, {synced:?}"
        );
        let log = OpLog::open(dir.path()).unwrap();
        assert!(log.ops().unwrap().iter().all(|op| op.payload != b"mine"));
        // told once: the handle holds the log as it is, which is synced
        held.sync().unwrap();
        sink.fsync().unwrap();
        assert_eq!(held.len(), 3, "deferred: {deferred}");
    }
}

#[test]
fn what_a_sink_was_left_to_sync_it_syncs_also_once_its_handle_has_read_the_log_anew() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    let given = collecting(&mut log);
    log.set_defer_durability(true);
    let sink = log.sync_handle().unwrap();
    log.append_with("a", 1, b"tx", &linked()).unwrap();
    log.sync().unwrap();
    log.reopen().unwrap();
    sink.fsync().unwrap();
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(log.appended_len()));
    let given: Vec<Vec<u8>> = given
        .lock()
        .unwrap()
        .iter()
        .map(|op| op.payload.clone())
        .collect();
    assert_eq!(given, [b"tx".to_vec()]);
}

#[test]
fn what_a_handle_leaves_to_its_sink_anew_is_synced_while_the_sink_tells_of_what_was_cut_before() {
    let dir = alt_os::temp::tempdir().unwrap();
    // its sink is left to sync an op of the handle, which is then cut
    let mut held = held_past_mine(dir.path(), true, false);
    let sink = held.sync_handle().unwrap();
    held.sync().unwrap();
    // the handle reads the log anew by itself, and nobody is told
    assert_eq!(held.settle(all).unwrap(), None);
    assert_eq!(held.len(), 2);
    held.append("a", 9, b"more").unwrap();
    // the op it appended since is in the log: its sync is left to the
    // sink, which syncs it and tells, once, of the op that was cut
    held.sync().unwrap();
    let told = sink.fsync();
    assert!(matches!(told, Err(OpLogError::CutBack)), "{told:?}");
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(held.appended_len()));
    assert!(held.confirmed());
    sink.fsync().unwrap();
    held.sync().unwrap();
    sink.fsync().unwrap();
}

#[test]
fn a_handle_that_read_the_log_anew_by_itself_is_told_by_its_next_sync_that_its_op_was_cut() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut held = held_past_mine(dir.path(), deferred, false);
        let sink = held.sync_handle().unwrap();
        assert_eq!(held.settle(all).unwrap(), None);
        assert_eq!(held.len(), 2);
        let mut synced = held.sync();
        if deferred && synced.is_ok() {
            synced = sink.fsync();
        }
        assert!(
            matches!(synced, Err(OpLogError::CutBack)),
            "deferred: {deferred}, {synced:?}"
        );
        // once: there is nothing of the handle's left to tell of
        held.sync().unwrap();
        sink.fsync().unwrap();
        assert!(held.confirmed(), "deferred: {deferred}");
    }
}

#[test]
fn a_handle_whose_caller_was_told_of_the_cut_and_read_anew_is_not_told_again_by_its_sync() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut held = held_past_mine(dir.path(), false, false);
    assert!(matches!(held.refresh(), Err(OpLogError::CutBack)));
    held.reopen().unwrap();
    held.sync().unwrap();
    // nor is one that was told by the sync itself
    let dir = alt_os::temp::tempdir().unwrap();
    let mut held = held_past_mine(dir.path(), false, false);
    assert!(matches!(held.sync(), Err(OpLogError::CutBack)));
    held.reopen().unwrap();
    held.sync().unwrap();
    assert!(held.confirmed());
}
