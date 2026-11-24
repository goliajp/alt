//! A handle on a version 2 log that reads the log anew for a caller
//! which tells nobody that ops were cut.

use std::fs;
use std::path::Path;

use alt_oplog::{Op, OpLog, OpLogError};

use super::v2_log::{boot, grow, new_log};

/// Has another handle, which finds no note beside the log, cut the op
/// of the payload `from` and every op after it; how many.
pub fn cut_from(dir: &Path, from: &[u8]) -> usize {
    let _ = fs::remove_file(dir.join("tail"));
    let mut other = OpLog::open(dir).unwrap();
    let mut gone = false;
    let held = |op: &Op| {
        gone |= op.payload == from;
        Ok::<_, OpLogError>(!gone)
    };
    other.settle(held).unwrap().map_or(0, |cut| cut.ops)
}

#[test]
fn a_handle_read_anew_for_a_caller_that_tells_nobody_is_told_by_its_next_sync_that_its_op_was_cut()
{
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut held = new_log(dir.path());
    grow(&mut held, "a", 2);
    held.append("a", 3, b"mine").unwrap();
    std::fs::remove_file(dir.path().join("tail")).unwrap();
    let mut other = OpLog::open(dir.path()).unwrap();
    let but_mine = |op: &alt_oplog::Op| Ok::<_, OpLogError>(op.payload != b"mine");
    assert_eq!(other.settle(but_mine).unwrap().unwrap().ops, 1);
    drop(other);
    assert!(matches!(held.follow(), Err(OpLogError::CutBack)));
    held.reread().unwrap();
    assert_eq!(held.len(), 2);
    held.append("a", 4, b"next").unwrap();
    assert!(matches!(held.sync(), Err(OpLogError::CutBack)));
    // once, and the op appended since is synced by the sync after
    held.sync().unwrap();
    assert!(held.confirmed());
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(held.appended_len()));
}

#[test]
fn each_time_an_op_of_a_handle_is_cut_its_next_sync_tells_so() {
    // direct, left to the sink, and read anew by the handle itself
    for (deferred, by_itself) in [(false, false), (false, true), (true, false), (true, true)] {
        let case = format!("deferred: {deferred}, by itself: {by_itself}");
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        log.set_defer_durability(deferred);
        let sink = log.sync_handle().unwrap();
        for round in 0..3 {
            log.append("a", 3, b"mine").unwrap();
            assert_eq!(cut_from(dir.path(), b"mine"), 1);
            match by_itself {
                true => assert_eq!(log.settle(|_| Ok::<_, OpLogError>(true)).unwrap(), None),
                false => {
                    assert!(matches!(log.follow(), Err(OpLogError::CutBack)));
                    log.reread().unwrap();
                }
            }
            log.append("a", 4, b"next").unwrap();
            let mut told = log.sync();
            if deferred && told.is_ok() {
                told = sink.fsync();
            }
            assert!(
                matches!(told, Err(OpLogError::CutBack)),
                "{case}, round {round}: {told:?}"
            );
            // once, and the op appended since is synced
            log.sync().unwrap();
            sink.fsync().unwrap();
            assert!(log.confirmed(), "{case}, round {round}");
        }
        let log = OpLog::open(dir.path()).unwrap();
        assert!(log.ops().unwrap().iter().all(|op| op.payload != b"mine"));
    }
}

#[test]
fn one_sync_tells_of_every_op_of_a_handle_that_is_gone_by_then() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    log.append("a", 3, b"mine").unwrap();
    assert_eq!(cut_from(dir.path(), b"mine"), 1);
    assert!(matches!(log.follow(), Err(OpLogError::CutBack)));
    log.reread().unwrap();
    // another op of the handle is cut, which the handle has not
    // found: one that ended past where the file ends now
    log.append("a", 4, &[7; 900]).unwrap();
    assert_eq!(cut_from(dir.path(), &[7; 900]), 1);
    assert!(fs::metadata(dir.path().join("log")).unwrap().len() < log.appended_len());
    assert!(matches!(log.sync(), Err(OpLogError::CutBack)));
    log.sync().unwrap();
    assert!(log.confirmed());
}

#[test]
fn an_op_of_a_handle_that_was_cut_is_told_of_though_the_handle_cuts_what_an_append_left_after() {
    for way in ["sync", "settle", "append"] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        // an op that ends well past where the log ends once it is cut
        log.append("a", 3, &[7; 900]).unwrap();
        assert_eq!(cut_from(dir.path(), &[7; 900]), 1);
        // an append that was never finished, and a cut of it that died
        // with nothing but the header of the log telling of it
        let path = dir.path().join("log");
        let mut file = std::fs::OpenOptions::new().append(true).open(path);
        std::io::Write::write_all(file.as_mut().unwrap(), &[0xa5; 100]).unwrap();
        let mut other = OpLog::open(dir.path()).unwrap();
        alt_oplog::v2::cut_dies_after(Some(1));
        let died = other.refresh();
        alt_oplog::v2::cut_dies_after(None);
        assert!(matches!(died, Err(OpLogError::Io(_))), "{way}: {died:?}");
        drop(other);
        // the handle reads the log anew, to its end, and cuts what was
        // left there itself: its own op was cut before, by another
        match way {
            "settle" => assert_eq!(log.settle(|_| Ok::<_, OpLogError>(true)).unwrap(), None),
            "append" => {
                assert!(matches!(log.follow(), Err(OpLogError::CutBack)));
                log.reread().unwrap();
                log.append("a", 4, b"more").unwrap();
            }
            _ => {}
        }
        assert!(matches!(log.sync(), Err(OpLogError::CutBack)), "{way}");
        log.sync().unwrap();
        assert!(log.confirmed(), "{way}");
    }
}

/// Leaves `len` bytes of no frame where the log in `dir` ends, and has a
/// writer that would cut them die with nothing but the header of the
/// log telling of the cut.
fn cut_of_an_unfinished_append_dies(dir: &Path, len: usize) {
    let mut file = fs::OpenOptions::new().append(true).open(dir.join("log"));
    std::io::Write::write_all(file.as_mut().unwrap(), &vec![0xa5; len]).unwrap();
    let mut other = OpLog::open(dir).unwrap();
    alt_oplog::v2::cut_dies_after(Some(1));
    let died = other.refresh();
    alt_oplog::v2::cut_dies_after(None);
    assert!(matches!(died, Err(OpLogError::Io(_))), "{died:?}");
}

#[test]
fn a_handle_whose_caller_was_told_tells_of_no_cut_again_for_an_op_it_could_not_look_at_then() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    // an op that ends well past where the log ends once it is cut
    log.append("a", 3, &[7; 600]).unwrap();
    assert_eq!(cut_from(dir.path(), &[7; 600]), 1);
    cut_of_an_unfinished_append_dies(dir.path(), 100);
    // the log ends for the handle where that cut was begun, short of
    // where its op ended: whether the op is cut it cannot tell
    assert!(matches!(log.follow(), Err(OpLogError::CutBack)));
    log.reopen().unwrap();
    log.refresh().unwrap();
    log.sync().unwrap();
    assert!(log.confirmed());
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(log.appended_len()));
}

#[test]
fn an_op_a_caller_was_told_of_while_a_cut_of_it_was_begun_is_synced_where_it_stays_in_the_log() {
    for cut_after in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        log.append("a", 3, b"mine").unwrap();
        // a cut of it dies with the header of the log alone telling of it
        fs::remove_file(dir.path().join("tail")).unwrap();
        let mut other = OpLog::open(dir.path()).unwrap();
        alt_oplog::v2::cut_dies_after(Some(1));
        let died = other.settle(|op| Ok::<_, OpLogError>(op.payload != b"mine"));
        alt_oplog::v2::cut_dies_after(None);
        assert!(matches!(died, Err(OpLogError::Io(_))), "{died:?}");
        drop(other);
        assert!(matches!(log.follow(), Err(OpLogError::CutBack)));
        log.reopen().unwrap();
        assert_eq!(log.len(), 2);
        if cut_after {
            // cut for good now: its caller was told once already
            assert_eq!(cut_from(dir.path(), b"mine"), 1);
        }
        log.sync().unwrap();
        assert!(log.confirmed(), "cut after: {cut_after}");
        assert_eq!(log.len(), if cut_after { 2 } else { 3 });
        assert_eq!(alt_oplog::bounded_in(dir.path()), Some(log.appended_len()));
    }
}
