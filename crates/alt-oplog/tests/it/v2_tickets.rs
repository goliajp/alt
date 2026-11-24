//! Who is told that frames of a version 2 log were cut: the caller whose
//! frames they are, by the ticket it holds for them, and nobody else.
//! No frame that was cut is forgotten before its holder was told, and
//! one whose holder never asks fails no sync of another.

use std::path::Path;

use alt_oplog::{Answer, OpLog, OpLogError, OpLogSink, Ticket};

use super::v2_log::{boot, grow, new_log};
use super::v2_reread::cut_from;

fn cut_back<T: std::fmt::Debug>(told: &Result<T, OpLogError>) -> bool {
    matches!(told, Err(OpLogError::CutBack))
}

/// Appends an op of `payload` for a caller of its own, and gives the
/// ticket of that caller.
fn asked(log: &mut OpLog, payload: &[u8]) -> Ticket {
    log.append("a", 1, payload).unwrap();
    let ticket = log.ticket();
    assert!(!ticket.is_empty());
    ticket
}

fn payloads(dir: &Path) -> Vec<Vec<u8>> {
    let log = OpLog::open_read_only(dir, None).unwrap();
    let ops = log.ops().unwrap();
    ops.iter().map(|op| op.payload.clone()).collect()
}

/// A handle of boot 1 on a new log of two synced ops, which syncs by
/// itself or leaves that to the sink given with it.
fn shared(dir: &Path, deferred: bool) -> (OpLog, OpLogSink) {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    log.set_defer_durability(deferred);
    let sink = log.sync_handle().unwrap();
    (log, sink)
}

/// What the holder of `ticket` is answered: by the handle, and by its
/// sink where the handle left the frames to it.
fn answered(
    log: &mut OpLog,
    sink: &OpLogSink,
    deferred: bool,
    ticket: &Ticket,
) -> Result<Answer, OpLogError> {
    log.sync_for(ticket)?;
    match deferred {
        true => sink.fsync_for(ticket),
        false => Ok(Answer::Synced),
    }
}

#[test]
fn a_caller_whose_op_was_cut_is_told_so_by_its_ticket_and_one_whose_op_is_in_the_log_is_not() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, sink) = shared(dir.path(), deferred);
        let first = asked(&mut log, b"first");
        let second = asked(&mut log, b"second");
        assert_eq!(cut_from(dir.path(), b"second"), 1);
        // the one whose op is in the log is answered that it is synced,
        // whichever of the two asks first
        let told = answered(&mut log, &sink, deferred, &second);
        assert!(cut_back(&told), "deferred: {deferred}, {told:?}");
        let synced = answered(&mut log, &sink, deferred, &first);
        assert_eq!(synced.unwrap(), Answer::Synced, "deferred: {deferred}");
        assert_eq!(alt_oplog::bounded_in(dir.path()), Some(log.appended_len()));
        assert_eq!(payloads(dir.path()).len(), 3);
        // told once: the ticket is spent
        let again = answered(&mut log, &sink, deferred, &second);
        assert_eq!(again.unwrap(), Answer::Synced, "deferred: {deferred}");
    }
}

#[test]
fn every_caller_whose_op_was_cut_is_told_so_and_the_order_they_ask_in_changes_nothing() {
    for (deferred, first_asks_first) in [(false, true), (false, false), (true, true), (true, false)]
    {
        let case = format!("deferred: {deferred}, the first asks first: {first_asks_first}");
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, sink) = shared(dir.path(), deferred);
        let first = asked(&mut log, b"first");
        let second = asked(&mut log, b"second");
        assert_eq!(cut_from(dir.path(), b"first"), 2);
        let order = match first_asks_first {
            true => [&first, &second],
            false => [&second, &first],
        };
        for ticket in order {
            let told = answered(&mut log, &sink, deferred, ticket);
            assert!(cut_back(&told), "{case}, {told:?}");
        }
        assert_eq!(payloads(dir.path()).len(), 2, "{case}");
    }
}

#[test]
fn a_caller_is_not_told_of_a_cut_for_the_op_of_another_whose_sync_was_left_to_the_sink() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (mut log, sink) = shared(dir.path(), true);
    let first = asked(&mut log, b"first");
    log.sync_for(&first).unwrap();
    assert_eq!(cut_from(dir.path(), b"first"), 1);
    // the handle reads the log anew, and nobody is told
    assert!(cut_back(&log.follow()));
    log.reread().unwrap();
    let second = asked(&mut log, b"second");
    let synced = answered(&mut log, &sink, true, &second);
    assert_eq!(synced.unwrap(), Answer::Synced);
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(log.appended_len()));
    // whoever waits for the first is told, by the sink it waits on
    assert!(cut_back(&sink.fsync_for(&first)));
}

#[test]
fn an_op_on_a_ticket_nobody_asked_with_is_told_of_when_its_holder_asks_whatever_was_synced_since() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, sink) = shared(dir.path(), deferred);
        let early = asked(&mut log, b"early");
        assert_eq!(cut_from(dir.path(), b"early"), 1);
        // callers come and go, with tickets and without
        for round in 0..3 {
            let ticket = asked(read_anew(&mut log), b"later");
            let synced = answered(&mut log, &sink, deferred, &ticket);
            assert_eq!(synced.unwrap(), Answer::Synced, "round {round}");
            log.append("a", 2, b"of the handle").unwrap();
            log.sync().unwrap();
            sink.fsync().unwrap();
            log.bound_now().unwrap();
        }
        assert!(log.confirmed());
        let told = answered(&mut log, &sink, deferred, &early);
        assert!(cut_back(&told), "deferred: {deferred}, {told:?}");
    }
}

/// The handle, having read the log anew where its ops were cut, with
/// nobody told.
fn read_anew(log: &mut OpLog) -> &mut OpLog {
    if cut_back(&log.follow()) {
        log.reread().unwrap();
    }
    log
}

#[test]
fn a_ticket_nobody_asks_with_fails_no_sync_of_another_caller() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, sink) = shared(dir.path(), deferred);
        let unclaimed = asked(&mut log, b"unclaimed");
        if deferred {
            // left to the sink, which nobody asks with the ticket
            log.sync_for(&unclaimed).unwrap();
        }
        assert_eq!(cut_from(dir.path(), b"unclaimed"), 1);
        for round in 0..4 {
            // with no ticket, as the one caller of the handle
            read_anew(&mut log).append("a", 2, b"more").unwrap();
            log.sync().unwrap();
            sink.fsync().unwrap();
            log.bound_now().unwrap();
            assert!(log.confirmed(), "deferred: {deferred}, round {round}");
        }
        drop(unclaimed);
    }
}

#[test]
fn one_fsync_that_finds_ops_cut_tells_every_caller_whose_ticket_holds_one_of_them() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (mut log, sink) = shared(dir.path(), true);
    let mut tickets = Vec::new();
    for payload in [&b"one"[..], b"two", b"three"] {
        let ticket = asked(&mut log, payload);
        log.sync_for(&ticket).unwrap();
        tickets.push(ticket);
    }
    assert_eq!(cut_from(dir.path(), b"two"), 2);
    // the fsync a leader makes for all of them
    let reached = sink.reached().unwrap();
    sink.fsync_to(reached).unwrap();
    assert!(cut_back(&sink.answer(&tickets[1])));
    assert!(cut_back(&sink.answer(&tickets[2])));
    // the op before them is in the log, with nothing left to sync it
    // up to: its caller has the handle leave it anew
    assert_eq!(sink.answer(&tickets[0]).unwrap(), Answer::Owed);
    log.sync_for(&tickets[0]).unwrap();
    assert_eq!(sink.fsync_for(&tickets[0]).unwrap(), Answer::Synced);
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(log.appended_len()));
    assert_eq!(payloads(dir.path()).len(), 3);
}

#[test]
fn tickets_joined_are_answered_as_one() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (mut log, sink) = shared(dir.path(), false);
    let mut both = asked(&mut log, b"first");
    both.join(asked(&mut log, b"second"));
    let other = asked(&mut log, b"third");
    assert_eq!(cut_from(dir.path(), b"second"), 2);
    assert!(cut_back(&answered(&mut log, &sink, false, &other)));
    assert!(cut_back(&answered(&mut log, &sink, false, &both)));
    // the first is in the log: with its ticket spent, the handle syncs it
    log.bound_now().unwrap();
    assert!(log.confirmed());
    assert_eq!(payloads(dir.path()).len(), 3);
}

#[test]
fn a_handle_that_cuts_an_op_on_a_ticket_itself_leaves_its_holder_to_be_told() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        log.append("a", 3, b"left").unwrap();
        drop(log);
        // another boot: an op is appended with no look and no sync, for
        // a caller that holds a ticket for it
        boot(2);
        let mut log = OpLog::open(dir.path()).unwrap();
        log.set_defer_durability(deferred);
        let sink = log.sync_handle().unwrap();
        let ticket = {
            let _unsynced = alt_os::durability::leave_unsynced(true);
            asked(&mut log, b"after")
        };
        // the handle looks, and cuts both
        let cut = log.settle(|op| Ok::<_, OpLogError>(op.payload != b"left"));
        assert_eq!(cut.unwrap().unwrap().ops, 2);
        let told = answered(&mut log, &sink, deferred, &ticket);
        assert!(cut_back(&told), "deferred: {deferred}, {told:?}");
        log.sync().unwrap();
        sink.fsync().unwrap();
    }
}

#[test]
fn a_sink_that_cannot_tell_whether_what_it_was_left_is_cut_tells_its_one_caller_once() {
    // a cut that died with its checkpoint laid over the first bytes of
    // the op, in a file as long as it was: the bytes the op ended with
    // are there
    for step in [2, 3] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        log.set_defer_durability(true);
        let sink = log.sync_handle().unwrap();
        log.append("a", 3, &[7; 900]).unwrap();
        log.sync().unwrap();
        let _ = std::fs::remove_file(dir.path().join("tail"));
        let mut cutter = OpLog::open(dir.path()).unwrap();
        alt_oplog::v2::cut_dies_after(Some(step));
        let cut = cutter.settle(|op| Ok::<_, OpLogError>(op.payload.len() < 900));
        alt_oplog::v2::cut_dies_after(None);
        assert!(
            matches!(cut, Err(OpLogError::Io(_))),
            "step {step}: {cut:?}"
        );
        drop(cutter);
        let told = sink.fsync();
        assert!(cut_back(&told), "step {step}: {told:?}");
        // the handle reads the log again and finds the op gone, of
        // which the caller was told
        log.sync().unwrap();
        sink.fsync().unwrap();
        assert_eq!(log.len(), 2, "step {step}");
        assert!(log.confirmed(), "step {step}");
    }
}
