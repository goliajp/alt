//! A version 2 log whose cut was refused for a frame to keep stays as it
//! is until it is repaired: no writer sets a boundary over the op that
//! was refused or appends after it, and a reader holds every op of the
//! log and is refused as a writer is.

use std::fs;
use std::path::Path;

use alt_oplog::{OpLog, OpLogError};

use super::v2_cut_back::but_left;
use super::v2_image::linked;
use super::v2_log::{boot, file_len, grow, new_log};
use super::v2_named::collecting;

/// A log of two synced ops, an op `left` that was never synced, and a
/// commit after it that was answered before the power went, as boot 2
/// finds it; where `left` and the commit start.
fn kept(dir: &Path) -> (u64, u64) {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        log.append("a", 3, b"left").unwrap();
    }
    let left = log.anchor().unwrap().start;
    log.append_with("a", 4, b"commit", &linked()).unwrap();
    log.sync().unwrap();
    boot(2);
    (left, log.anchor().unwrap().start)
}

fn refused(told: Result<impl std::fmt::Debug, OpLogError>, (left, commit): (u64, u64)) {
    match told {
        Err(OpLogError::Kept { unheld, kept }) => assert_eq!((unheld, kept), (left, commit)),
        other => panic!("{other:?}"),
    }
}

fn unjudged(told: Result<impl std::fmt::Debug, OpLogError>, left: u64, case: &str) {
    match told {
        Err(OpLogError::Unjudged { at }) => assert_eq!(at, left, "{case}"),
        other => panic!("{case}: {other:?}"),
    }
}

#[test]
fn a_writer_that_syncs_neither_appends_after_an_op_that_was_refused_nor_sets_a_boundary_over_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let places = kept(dir.path());
    let len = file_len(dir.path());
    let mut looked = OpLog::open(dir.path()).unwrap();
    refused(looked.settle(but_left), places);
    // the handle that was refused, and one opened after it, which has
    // somebody to sync what ops name
    let mut after = OpLog::open(dir.path()).unwrap();
    let given = collecting(&mut after);
    for (case, log) in [("refused", &mut looked), ("opened after", &mut after)] {
        unjudged(log.append("a", 5, b"more"), places.0, case);
        unjudged(log.sync(), places.0, case);
        unjudged(log.bound_now(), places.0, case);
        assert!(!log.confirmed() && log.unsettled(), "{case}");
        // and a look comes to the same as before
        refused(log.settle(but_left), places);
    }
    assert!(given.lock().unwrap().is_empty());
    assert_eq!(file_len(dir.path()), len);
    // a writer that does not sync appends, as after any op nobody
    // looked at; no sync sets a boundary for that
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        after.append("a", 6, b"staged").unwrap();
    }
    unjudged(after.sync(), places.0, "after an append that is not synced");
    let frames_say = OpLog::open_read_only(dir.path(), None).unwrap();
    assert!(frames_say.bounded_to().unwrap() <= places.0);
}

#[test]
fn a_reader_of_a_log_whose_cut_is_refused_holds_every_op_and_is_refused_as_a_writer_is() {
    let dir = alt_os::temp::tempdir().unwrap();
    let places = kept(dir.path());
    let note = fs::read(dir.path().join("tail")).unwrap();
    let reader = OpLog::open_read_only(dir.path(), None).unwrap();
    assert_eq!(reader.len(), 4);
    refused(reader.unconfirmed(but_left), places);
    // it noted nothing: the log ends nowhere short of the commit for
    // whoever opens it after, reader or writer
    assert_eq!(fs::read(dir.path().join("tail")).unwrap(), note);
    for read_only in [true, false] {
        let opened = match read_only {
            true => OpLog::open_read_only(dir.path(), None).unwrap(),
            false => OpLog::open(dir.path()).unwrap(),
        };
        assert_eq!(opened.len(), 4, "read only: {read_only}");
        assert!(!opened.passed_over() && opened.unsettled());
        let commit = opened.ops().unwrap().last().unwrap().payload.clone();
        assert_eq!(commit, b"commit");
        refused(opened.unconfirmed(but_left), places);
    }
    // a reader that is told of an op with nothing to keep past it ends
    // the log there, as before
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    log.append("a", 3, b"left").unwrap();
    let left = log.anchor().unwrap().start;
    drop(log);
    boot(2);
    let reader = OpLog::open_read_only(dir.path(), None).unwrap();
    assert_eq!(reader.unconfirmed(but_left).unwrap().unwrap().at, left);
    assert_eq!(OpLog::open_read_only(dir.path(), None).unwrap().len(), 2);
}
