//! What a handle of a version 2 log writes for a caller is on the ticket
//! of that caller from when it is written, whoever else appends before
//! or after and in whatever order the callers ask; what it writes
//! otherwise is on none. A ticket that is dropped leaves its frames to
//! the handle, and one of another log is refused.

use alt_oplog::{Answer, OpLog, OpLogError};

use super::v2_log::{boot, grow, new_log};
use super::v2_reread::cut_from;
use super::v2_tickets::{answered, asked, cut_back, payloads, shared};

/// What the holder of `ticket` is told at last: by the handle, by its
/// sink where the handle left the frames to it, and by the handle once
/// more where the sink has nothing to sync them up to.
fn told(
    log: &mut OpLog,
    sink: &alt_oplog::OpLogSink,
    deferred: bool,
    ticket: &alt_oplog::Ticket,
) -> Result<(), OpLogError> {
    if answered(log, sink, deferred, ticket)? == Answer::Owed {
        log.sync_for(ticket)?;
        assert_eq!(sink.answer(ticket)?, Answer::Synced);
    }
    Ok(())
}

#[test]
fn of_two_callers_that_appended_in_turn_the_one_whose_op_was_cut_is_told_and_the_other_is_not() {
    for (deferred, first_asks_first) in [(false, true), (false, false), (true, true), (true, false)]
    {
        let case = format!("deferred: {deferred}, the first asks first: {first_asks_first}");
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, sink) = shared(dir.path(), deferred);
        let first = asked(&mut log, b"first");
        let second = asked(&mut log, b"second");
        assert_eq!(cut_from(dir.path(), b"second"), 1);
        let mut answers = [None, None];
        let order = match first_asks_first {
            true => [0, 1],
            false => [1, 0],
        };
        for caller in order {
            let ticket = [&first, &second][caller];
            answers[caller] = Some(told(&mut log, &sink, deferred, ticket));
        }
        let [of_first, of_second] = answers.map(Option::unwrap);
        assert!(of_first.is_ok(), "{case}: {of_first:?}");
        assert!(cut_back(&of_second), "{case}: {of_second:?}");
        assert_eq!(payloads(dir.path()).len(), 3, "{case}");
    }
}

#[test]
fn what_a_caller_without_a_ticket_had_appended_is_on_no_ticket_written_after() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, sink) = shared(dir.path(), deferred);
        log.append("a", 1, b"of the handle").unwrap();
        let other = asked(&mut log, b"of a caller with a ticket");
        // the two are cut, and each caller is told of its own
        assert_eq!(cut_from(dir.path(), b"of the handle"), 2);
        let of_other = told(&mut log, &sink, deferred, &other);
        assert!(cut_back(&of_other), "deferred: {deferred}, {of_other:?}");
        let of_handle = log.sync().and_then(|()| sink.fsync());
        assert!(cut_back(&of_handle), "deferred: {deferred}, {of_handle:?}");
        // once
        log.sync().unwrap();
        sink.fsync().unwrap();
        assert!(log.confirmed());
    }
}

#[test]
fn that_an_op_of_the_handle_was_found_cut_is_told_to_its_caller_and_to_no_ticket_written_after() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, sink) = shared(dir.path(), deferred);
        log.append("a", 1, b"of the handle").unwrap();
        assert_eq!(cut_from(dir.path(), b"of the handle"), 1);
        // the handle reads the log anew, and nobody is told
        assert!(cut_back(&log.follow()));
        log.reread().unwrap();
        let other = asked(&mut log, b"of a caller with a ticket");
        told(&mut log, &sink, deferred, &other).unwrap();
        let of_handle = log.sync().and_then(|()| sink.fsync());
        assert!(cut_back(&of_handle), "deferred: {deferred}, {of_handle:?}");
    }
}

#[test]
fn a_ticket_that_is_dropped_leaves_its_ops_to_whoever_asks_the_handle_without_one() {
    for (deferred, cut) in [(false, false), (false, true), (true, false), (true, true)] {
        let case = format!("deferred: {deferred}, cut: {cut}");
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, sink) = shared(dir.path(), deferred);
        drop(asked(&mut log, b"nobody waits"));
        // so does the ticket of what a caller began to write and gave up
        let gave_up = log.ticketed(|log| {
            log.append("a", 1, b"given up")?;
            Err::<(), OpLogError>(OpLogError::TooLarge(0))
        });
        assert!(gave_up.is_err());
        if cut {
            assert_eq!(cut_from(dir.path(), b"given up"), 1, "{case}");
        }
        let of_handle = log.sync().and_then(|()| sink.fsync());
        assert_eq!(cut_back(&of_handle), cut, "{case}: {of_handle:?}");
        log.sync().unwrap();
        sink.fsync().unwrap();
        assert!(log.confirmed(), "{case}");
        assert_eq!(payloads(dir.path()).len(), 4 - cut as usize, "{case}");
    }
}

#[test]
fn a_caller_that_wrote_nothing_holds_a_ticket_for_nothing() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (mut log, sink) = shared(dir.path(), true);
    let (len, ticket) = log.ticketed(|log| Ok::<_, OpLogError>(log.len())).unwrap();
    assert!(ticket.is_empty() && len == 2);
    log.sync_for(&ticket).unwrap();
    assert_eq!(sink.answer(&ticket).unwrap(), Answer::Synced);
    // and what is appended once it has its ticket is the handle's
    log.append("a", 1, b"of the handle").unwrap();
    assert_eq!(cut_from(dir.path(), b"of the handle"), 1);
    assert_eq!(sink.fsync_for(&ticket).unwrap(), Answer::Synced);
    assert!(cut_back(&log.sync().and_then(|()| sink.fsync())));
}

#[test]
fn a_ticket_is_good_for_the_log_it_was_written_on_and_for_no_other() {
    let (dir, other) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (mut log, _sink) = shared(dir.path(), false);
    let ticket = asked(&mut log, b"here");
    boot(1);
    let mut elsewhere = new_log(other.path());
    grow(&mut elsewhere, "b", 1);
    let sink = elsewhere.sync_handle().unwrap();
    let foreign = |told: Result<(), OpLogError>| matches!(told, Err(OpLogError::Format(_)));
    assert!(foreign(elsewhere.sync_for(&ticket)));
    assert!(foreign(sink.answer(&ticket).map(drop)));
    assert!(foreign(sink.fsync_for(&ticket).map(drop)));
    log.sync_for(&ticket).unwrap();
    let _: OpLog = elsewhere;
}

#[test]
fn a_sync_that_is_told_to_promise_nothing_leaves_its_caller_owed_no_answer() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut log, sink) = shared(dir.path(), deferred);
        let ticket = {
            let _unsynced = alt_os::durability::leave_unsynced(true);
            log.append("a", 1, b"of the handle").unwrap();
            let ticket = asked(&mut log, b"of a caller with a ticket");
            log.sync().unwrap();
            log.sync_for(&ticket).unwrap();
            ticket
        };
        assert_eq!(cut_from(dir.path(), b"of the handle"), 2);
        // neither was promised anything: no later sync tells of them
        told(&mut log, &sink, deferred, &ticket).unwrap();
        for _ in 0..2 {
            log.sync().unwrap();
            sink.fsync().unwrap();
        }
        assert!(log.confirmed(), "deferred: {deferred}");
    }
}
