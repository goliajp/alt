//! What is left unfinished at the end of a version 2 log is cut off by
//! whoever reads the log to append: that is no look at the ops before it,
//! and the ones another boot left stay to be looked at.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use alt_oplog::{Meta, Op, OpLog, OpLogError};

use super::v2_bounds::stage;
use super::v2_log::{boot, file_len, grow, new_log};

fn but_left(op: &Op) -> Result<bool, OpLogError> {
    Ok(op.payload != b"left")
}

/// In boot 1: two synced ops, a ref transaction `left` that is not, and
/// three bytes of an append that was never finished. Handles opened after
/// it run in boot 2. Returns where `left` starts.
fn left_and_torn(dir: &Path) -> u64 {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    let linked = Meta {
        linked: true,
        ..Meta::default()
    };
    log.append_with("a", 3, b"left", &linked).unwrap();
    let at = log.anchor().unwrap().start;
    drop(log);
    tear(dir);
    boot(2);
    at
}

fn tear(dir: &Path) {
    let mut bytes = fs::read(dir.join("log")).unwrap();
    bytes.extend_from_slice(&[1, 2, 3]);
    fs::write(dir.join("log"), bytes).unwrap();
}

/// Holds every handle opened now to having `left` to look at, and a
/// writer's look to cutting it with the `after` ops appended after it.
fn still_left(dir: &Path, at: u64, after: usize) {
    for read_only in [true, false] {
        let log = match read_only {
            true => OpLog::open_read_only(dir, None).unwrap(),
            false => OpLog::open(dir).unwrap(),
        };
        assert!(log.unsettled(), "read only: {read_only}");
        let found = log.unconfirmed(but_left).unwrap().unwrap();
        assert_eq!((found.at, found.ops), (at, 1 + after));
    }
    let mut writer = OpLog::open(dir).unwrap();
    let cut = writer.settle(but_left).unwrap().unwrap();
    assert_eq!((cut.at, cut.ops, writer.len()), (at, 1 + after, 2));
    assert!(!OpLog::open(dir).unwrap().unsettled());
}

#[test]
fn a_look_that_first_cuts_what_was_never_finished_looks_at_the_ops_before_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let at = left_and_torn(dir.path());
    let mut log = OpLog::open(dir.path()).unwrap();
    assert!(log.unsettled() && log.passed_over());
    let cut = log.settle(but_left).unwrap().unwrap();
    assert_eq!((cut.at, cut.ops, log.len()), (at, 1, 2));
    let told = log.take_discarded();
    assert_eq!((told.len(), told[0].unread, told[1].ops.len()), (2, 3, 1));
}

#[test]
fn reading_on_past_what_was_never_finished_leaves_the_ops_before_it_to_be_looked_at() {
    let dir = alt_os::temp::tempdir().unwrap();
    let at = left_and_torn(dir.path());
    let torn = file_len(dir.path());
    let mut log = OpLog::open(dir.path()).unwrap();
    log.refresh().unwrap();
    assert_eq!(log.take_discarded()[0].unread, 3);
    assert!(file_len(dir.path()) != torn && log.unsettled());
    still_left(dir.path(), at, 0);
}

#[test]
fn staging_after_what_was_never_finished_leaves_the_ops_before_it_to_be_looked_at() {
    let dir = alt_os::temp::tempdir().unwrap();
    let at = left_and_torn(dir.path());
    let mut staging = OpLog::open(dir.path()).unwrap();
    stage(&mut staging, 40);
    assert_eq!(staging.take_discarded()[0].unread, 3);
    assert!(staging.unsettled());
    drop(staging);
    assert!(file_len(dir.path()) > 2 * 4096);
    still_left(dir.path(), at, 40);
}

#[test]
fn a_commit_after_what_was_never_finished_has_the_ops_before_it_seen_to() {
    for stored in [true, false] {
        let dir = alt_os::temp::tempdir().unwrap();
        let at = left_and_torn(dir.path());
        let mut log = OpLog::open(dir.path()).unwrap();
        let given = Arc::new(Mutex::new(Vec::new()));
        let seen = given.clone();
        log.sync_named_with(Arc::new(move |ops| {
            let mut seen = seen.lock().unwrap();
            seen.extend(ops.iter().map(|op| op.payload.clone()));
            match stored {
                true => Ok(()),
                false => Err(std::io::Error::other("what it names is not there")),
            }
        }));
        let appended = log.append("c", 4, b"committed");
        assert_eq!(*given.lock().unwrap(), [b"left"]);
        if stored {
            appended.unwrap();
            assert!(!OpLog::open(dir.path()).unwrap().unsettled());
            continue;
        }
        assert!(matches!(appended, Err(OpLogError::Io(_))));
        still_left(dir.path(), at, 0);
    }
}

#[test]
fn a_note_that_ops_are_left_stands_when_what_was_never_finished_is_cut_after_it() {
    // a reader noted the op it would not vouch for
    let dir = alt_os::temp::tempdir().unwrap();
    let at = left_and_torn(dir.path());
    let reader = OpLog::open_read_only(dir.path(), None).unwrap();
    assert!(reader.unconfirmed(but_left).unwrap().is_some());
    OpLog::open(dir.path()).unwrap().refresh().unwrap();
    still_left(dir.path(), at, 0);

    // a writer that only stages noted that it looked at nothing, over
    // blocks whose checkpoints are of this boot
    let dir = alt_os::temp::tempdir().unwrap();
    let at = left_and_torn(dir.path());
    let mut staging = OpLog::open(dir.path()).unwrap();
    stage(&mut staging, 40);
    drop(staging);
    tear(dir.path());
    let mut log = OpLog::open(dir.path()).unwrap();
    assert!(log.recent().len() < 20 && log.unsettled());
    log.refresh().unwrap();
    assert_eq!(log.take_discarded().len(), 1);
    assert!(log.unsettled());
    still_left(dir.path(), at, 40);
}

fn but_later(op: &Op) -> Result<bool, OpLogError> {
    Ok(op.payload != b"later")
}

/// In boot 1 two synced ops and two ref transactions that are not,
/// `left` and `later`; in a later boot a reader that vouches for the
/// first and not for the second, and notes so. With `cut_after`, what was
/// never finished is cut off the log after them in a boot between.
/// Returns where `later` starts.
fn left_and_later_looked_at(dir: &Path, cut_after: bool) -> u64 {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    let linked = Meta {
        linked: true,
        ..Meta::default()
    };
    log.append_with("a", 3, b"left", &linked).unwrap();
    log.append_with("a", 4, b"later", &linked).unwrap();
    let at = log.anchor().unwrap().start;
    drop(log);
    boot(2);
    if cut_after {
        tear(dir);
        OpLog::open(dir).unwrap().refresh().unwrap();
        boot(3);
    }
    let reader = OpLog::open_read_only(dir, None).unwrap();
    assert_eq!(reader.unconfirmed(but_later).unwrap().unwrap().at, at);
    at
}

#[test]
fn a_commit_has_the_ops_seen_to_that_somebody_vouched_for_and_nobody_synced() {
    let dir = alt_os::temp::tempdir().unwrap();
    left_and_later_looked_at(dir.path(), false);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 3, "the log ends before the op not vouched for");
    let given = Arc::new(Mutex::new(Vec::new()));
    let seen = given.clone();
    log.sync_named_with(Arc::new(move |ops| {
        let mut seen = seen.lock().unwrap();
        seen.extend(ops.iter().map(|op| op.payload.clone()));
        Ok(())
    }));
    log.append("c", 5, b"committed").unwrap();
    // that an op's content is there does not say it is synced
    assert_eq!(*given.lock().unwrap(), [&b"left"[..], b"later"]);
    assert_eq!(
        alt_oplog::bounded_in(dir.path()),
        Some(log.anchor().unwrap().start)
    );
}

#[test]
fn a_look_at_a_log_a_reader_ended_early_is_no_look_at_what_lies_past_that_end() {
    let dir = alt_os::temp::tempdir().unwrap();
    // the log was cut since: the note is of another generation than
    // the log has where the reader ended it, and says nothing but that
    let at = left_and_later_looked_at(dir.path(), true);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert!(log.len() == 3 && log.unsettled());
    let mut asked = 0;
    let found = log.unconfirmed(|_| {
        asked += 1;
        Ok::<_, OpLogError>(true)
    });
    assert_eq!((found.unwrap(), asked), (None, 1));
    assert!(log.unsettled());
    // as a writer it reads on, and has that op to look at
    let cut = log.settle(but_later).unwrap().unwrap();
    assert_eq!((cut.at, cut.ops, log.len()), (at, 1, 3));
}
