//! The note beside a version 2 log says what it says of that log: one
//! written for another log is taken for gone, and so is one that names
//! a place the file does not reach, unless it says, read under the
//! lock, that the log is synced past the end of the file: that log lost
//! what was synced.

use std::fs::{self, TryLockError};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use alt_oplog::{OpLog, OpLogError};

use super::v2_log::{boot, grow, new_log};
use super::v2_named::collecting;
use super::v2_note_fields::tail;

#[test]
fn the_note_of_another_log_put_beside_a_log_says_nothing_of_it() {
    let (dir, other) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    boot(1);
    // another log of this boot, synced further than this one is and no
    // further than this one is long
    let mut far = new_log(other.path());
    grow(&mut far, "b", 3);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    let synced = log.appended_len();
    let given = collecting(&mut log);
    log.append_with("a", 3, &[7; 600], &super::v2_image::linked())
        .unwrap();
    let end = log.appended_len();
    assert!(synced < far.appended_len() && far.appended_len() < end);
    fs::copy(other.path().join("tail"), dir.path().join("tail")).unwrap();
    assert_eq!(alt_oplog::bounded_in(dir.path()), None);
    assert_eq!(log.bounded_to(), Some(synced));
    assert!(!log.confirmed());
    // whoever opens the log has what is past the boundary looked at
    assert!(OpLog::open_read_only(dir.path(), None).unwrap().unsettled());
    // a sync does its work, and notes it of this log
    log.sync().unwrap();
    assert_eq!(given.lock().unwrap().len(), 1);
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(end));
    assert!(log.confirmed());
}

/// What a handle is told that is held to a boundary past where the
/// log ends: synced up to `synced`, it holds whole frames up to `holds`.
fn lost(told: Result<(), OpLogError>, synced: u64, holds: u64) {
    match told {
        Err(OpLogError::Lost {
            synced: to,
            holds: has,
        }) => {
            assert_eq!((to, has), (synced, holds))
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_log_made_shorter_than_it_is_noted_as_synced_is_refused_by_a_writer_that_opens_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    let ends = grow(&mut log, "a", 3);
    drop(log);
    // something other than a cut made the file end before the boundary
    // the note names: no cut is made within what is synced
    let file = fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("log"))
        .unwrap();
    file.set_len(ends[1].end).unwrap();
    let (synced, holds) = (ends[2].end, ends[1].end);
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(synced));
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 2);
    lost(log.check_synced(), synced, holds);
    lost(
        log.settle(|_| Ok::<_, OpLogError>(true)).map(drop),
        synced,
        holds,
    );
    lost(log.append("a", 9, b"next").map(drop), synced, holds);
    assert_eq!(fs::metadata(dir.path().join("log")).unwrap().len(), holds);
}

#[test]
fn a_note_that_names_a_place_past_the_end_of_the_file_and_no_boundary_there_says_nothing() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    let ends = grow(&mut log, "a", 3);
    let len = log.appended_len();
    drop(log);
    // synced as it is, and looked at or ending for a reader past the
    // end of the file: no writer of this log left that
    for (judged, hidden, at) in [(len + 1, 0, 0), (len, len, len + 1)] {
        let note = tail(dir.path(), 1, 0, false, [ends[1].end, judged, hidden, at]);
        fs::write(dir.path().join("tail"), note).unwrap();
        assert_eq!(alt_oplog::bounded_in(dir.path()), None);
        let log = OpLog::open(dir.path()).unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log.bounded_to(), Some(ends[1].end));
        assert!(log.unsettled());
        log.check_synced().unwrap();
    }
}

#[test]
fn a_note_that_names_a_place_past_the_end_of_the_file_is_read_again_under_the_lock() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    let ends = grow(&mut log, "a", 3);
    let len = log.appended_len();
    drop(log);
    // as a read finds it that meets a cut between the note it wrote and
    // the file it makes shorter: the note names the log as it is once
    // cut, which the read under the lock finds in a file that long
    let kept = fs::read(dir.path().join("tail")).unwrap();
    let past = tail(dir.path(), 1, 0, false, [len + 100, len + 100, 0, 0]);
    fs::write(dir.path().join("tail"), past).unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let (read, as_it_is) = (reads.clone(), kept.clone());
    alt_oplog::v2::before_note_read(Some(Box::new(move |dir| {
        // before the second read, which is made with the lock held
        if read.fetch_add(1, Ordering::Relaxed) == 1 {
            let lock = fs::File::open(dir.join("lock")).unwrap();
            let held = matches!(lock.try_lock(), Err(TryLockError::WouldBlock));
            assert!(held, "the note is read again without the lock");
            fs::write(dir.join("tail"), &as_it_is).unwrap();
        }
    })));
    let bounded = alt_oplog::bounded_in(dir.path());
    alt_oplog::v2::before_note_read(None);
    assert_eq!(
        (bounded, reads.load(Ordering::Relaxed)),
        (Some(ends[2].end), 2)
    );
}

#[test]
fn a_log_that_ends_short_of_what_was_synced_is_refused_by_the_handle_that_synced_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = new_log(dir.path());
    let anchors = grow(&mut log, "a", 3);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("log"))
        .unwrap();
    file.set_len(anchors[1].end).unwrap();
    // the handle that synced the log knows how far it was synced, as
    // the note beside the log does, and is held to that once it has
    // read the log anew
    assert!(matches!(log.refresh(), Err(OpLogError::CutBack)));
    log.reopen().unwrap();
    assert_eq!(log.len(), 2);
    let lost = |said: Result<(), OpLogError>| match said {
        Err(OpLogError::Lost { synced, holds }) => {
            assert_eq!((synced, holds), (anchors[2].end, anchors[1].end))
        }
        other => panic!("{other:?}"),
    };
    lost(log.check_synced());
    lost(log.append("a", 1, b"x").map(|_| ()));
    lost(log.settle(|_| Ok::<_, OpLogError>(true)).map(|_| ()));
    assert_eq!(
        fs::metadata(dir.path().join("log")).unwrap().len(),
        anchors[1].end
    );
}
