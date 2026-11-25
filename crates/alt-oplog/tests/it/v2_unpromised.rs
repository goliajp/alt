//! A sync of a version 2 log that is told to leave what is written
//! unsynced promises nothing, and is owed for nothing after, of the
//! frames that were written so told. A handle is shared by callers that
//! are told so and callers that are not: what a writer that syncs wrote
//! is owed an answer through such a sync, and so is a frame that was
//! found cut.

use alt_oplog::{OpLog, OpLogError, OpLogSink};

use super::v2_log::{boot, grow, new_log};
use super::v2_reread::cut_from;

/// A handle on a new log of two synced ops, and its sink.
fn shared(dir: &std::path::Path, deferred: bool) -> (OpLog, OpLogSink) {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    log.set_defer_durability(deferred);
    let sink = log.sync_handle().unwrap();
    (log, sink)
}

/// Syncs, sets a boundary or syncs for `ticket` as a caller does that
/// is told to leave what is written unsynced.
fn unpromised(log: &mut OpLog, how: &str, ticket: Option<&alt_oplog::Ticket>) {
    let _unsynced = alt_os::durability::leave_unsynced(true);
    match (how, ticket) {
        (_, Some(ticket)) => log.sync_for(ticket).unwrap(),
        ("bound", _) => log.bound_now().unwrap(),
        _ => log.sync().unwrap(),
    }
}

fn cut_back<T: std::fmt::Debug>(told: Result<T, OpLogError>, case: &str) {
    assert!(matches!(told, Err(OpLogError::CutBack)), "{case}: {told:?}");
}

#[test]
fn an_op_left_to_a_sink_that_was_cut_is_told_of_by_the_sink_though_a_sync_promised_nothing_since() {
    for how in ["sync", "bound"] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, sink) = shared(dir.path(), true);
        log.append("a", 3, b"mine").unwrap();
        log.sync().unwrap();
        assert_eq!(cut_from(dir.path(), b"mine"), 1);
        unpromised(&mut log, how, None);
        cut_back(sink.fsync(), how);
        sink.fsync().unwrap();
    }
}

#[test]
fn an_op_of_a_writer_that_syncs_is_owed_an_answer_through_a_sync_that_promised_nothing() {
    for (how, found) in [("sync", false), ("bound", false), ("sync", true)] {
        let case = format!("{how}, found cut before: {found}");
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, _sink) = shared(dir.path(), false);
        log.append("a", 3, b"mine").unwrap();
        assert_eq!(cut_from(dir.path(), b"mine"), 1);
        if found {
            // the handle has found it cut, and nobody was told yet
            cut_back(log.follow(), &case);
            log.reread().unwrap();
        }
        unpromised(&mut log, how, None);
        cut_back(log.sync(), &case);
        log.sync().unwrap();
        assert!(log.confirmed(), "{case}");
    }
}

#[test]
fn an_op_on_a_ticket_of_a_writer_that_syncs_is_owed_an_answer_through_a_sync_that_promised_nothing()
{
    let dir = alt_os::temp::tempdir().unwrap();
    let (mut log, _sink) = shared(dir.path(), false);
    let (_, ticket) = log.ticketed(|log| log.append("a", 3, b"mine")).unwrap();
    unpromised(&mut log, "sync", Some(&ticket));
    assert_eq!(cut_from(dir.path(), b"mine"), 1);
    cut_back(log.sync_for(&ticket), "the ticket");
    log.sync().unwrap();
}

#[test]
fn of_ops_that_were_cut_a_sync_tells_for_the_one_a_writer_that_syncs_left_and_not_for_the_other() {
    for with_mine in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, _sink) = shared(dir.path(), false);
        {
            let _unsynced = alt_os::durability::leave_unsynced(true);
            log.append("a", 3, b"loose").unwrap();
        }
        if with_mine {
            log.append("a", 4, b"mine").unwrap();
        }
        assert_eq!(cut_from(dir.path(), b"loose"), 1 + with_mine as usize);
        // both are found cut before the caller that was promised
        // nothing asks
        cut_back(log.follow(), "read on");
        log.reread().unwrap();
        unpromised(&mut log, "sync", None);
        match with_mine {
            true => cut_back(log.sync(), "with an op of a writer that syncs"),
            false => log.sync().unwrap(),
        }
        log.sync().unwrap();
        assert!(log.confirmed(), "with mine: {with_mine}");
    }
}

#[test]
fn a_boundary_and_a_sync_that_promise_nothing_answer_for_the_same_frames() {
    for how in ["sync", "bound"] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, sink) = shared(dir.path(), true);
        {
            let _unsynced = alt_os::durability::leave_unsynced(true);
            log.append("a", 3, b"loose").unwrap();
        }
        // left to the sink by a caller that syncs, with nothing of its
        // own
        log.sync().unwrap();
        assert_eq!(cut_from(dir.path(), b"loose"), 1);
        unpromised(&mut log, how, None);
        sink.fsync().unwrap();
        log.sync().unwrap();
        sink.fsync().unwrap();
        assert!(log.confirmed(), "{how}");
    }
}
