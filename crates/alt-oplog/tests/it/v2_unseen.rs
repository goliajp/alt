//! A handle of a version 2 log that is given nobody to sync what ops
//! name records no sync over an op that names something.

use std::sync::Arc;

use alt_oplog::{OpLog, OpLogSink};

use super::v2_image::linked;
use super::v2_log::{boot, file_len, grow, new_log};
use super::v2_named::collecting;

/// The handle, given somebody to sync what ops name who finds all of it
/// stored.
pub fn seeing(mut log: OpLog) -> OpLog {
    log.sync_named_with(Arc::new(|_| Ok(())));
    log
}

/// Has a writer that somebody syncs for what ops name set a boundary
/// over all the log in `dir` holds.
pub fn bound(dir: &std::path::Path) {
    seeing(OpLog::open(dir).unwrap()).bound_now().unwrap();
}

/// A handle on a new log of two synced ops with nobody to sync what ops
/// name, which leaves its syncs to the sink given with it when
/// `deferred`; and where the log is synced up to.
fn unseen(dir: &std::path::Path, deferred: bool) -> (OpLog, OpLogSink, u64) {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    log.set_defer_durability(deferred);
    let sink = log.sync_handle().unwrap();
    let synced = log.appended_len();
    assert_eq!(alt_oplog::bounded_in(dir), Some(synced));
    (log, sink, synced)
}

#[test]
fn a_sync_with_nobody_to_see_to_what_ops_name_sets_no_boundary_over_a_ref_transaction() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, sink, synced) = unseen(dir.path(), deferred);
        log.append_with("a", 3, b"commit", &linked()).unwrap();
        // the frames are synced, and the caller is answered: no boundary
        // says that what the op names is
        for _ in 0..2 {
            log.sync().unwrap();
            sink.fsync().unwrap();
            assert_eq!(
                alt_oplog::bounded_in(dir.path()),
                Some(synced),
                "deferred: {deferred}"
            );
            assert!(!log.confirmed(), "deferred: {deferred}");
        }
        log.bound_now().unwrap();
        assert_eq!(alt_oplog::bounded_in(dir.path()), Some(synced));
        // once somebody sees to what ops name, the next sync gives it
        // the op and sets the boundary
        let given = collecting(&mut log);
        log.set_defer_durability(false);
        log.sync().unwrap();
        let given = given.lock().unwrap();
        assert_eq!(given.len(), 1, "deferred: {deferred}");
        assert_eq!(given[0].payload, b"commit");
        assert_eq!(
            alt_oplog::bounded_in(dir.path()),
            Some(file_len(dir.path()))
        );
        assert!(log.confirmed());
    }
}

#[test]
fn a_sync_with_nobody_to_see_to_what_ops_name_sets_its_boundary_over_ops_that_name_nothing() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, sink, _) = unseen(dir.path(), deferred);
        log.append("a", 3, b"plain").unwrap();
        log.sync().unwrap();
        sink.fsync().unwrap();
        assert_eq!(
            alt_oplog::bounded_in(dir.path()),
            Some(log.appended_len()),
            "deferred: {deferred}"
        );
        assert!(log.confirmed(), "deferred: {deferred}");
    }
}

#[test]
fn a_sink_made_before_its_handle_was_given_somebody_to_sync_what_ops_name_has_that_one_too() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (mut log, sink, synced) = unseen(dir.path(), true);
    let given = collecting(&mut log);
    log.append_with("a", 3, b"a ref transaction", &linked())
        .unwrap();
    log.sync().unwrap();
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(synced));
    // the fsync has what the op names seen to, and records the sync
    sink.fsync().unwrap();
    let given: Vec<Vec<u8>> = given
        .lock()
        .unwrap()
        .iter()
        .map(|op| op.payload.clone())
        .collect();
    assert_eq!(given, [b"a ref transaction".to_vec()]);
    assert_eq!(
        alt_oplog::bounded_in(dir.path()),
        Some(file_len(dir.path()))
    );
    assert!(log.confirmed());
}
