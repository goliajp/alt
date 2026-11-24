//! The note beside a version 2 log says what it says of that log: one
//! written for another log, or one that names a place the file does not
//! reach, is taken for gone.

use std::fs;

use alt_oplog::{OpLog, OpLogError};

use super::v2_log::{boot, grow, new_log};
use super::v2_named::collecting;

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

#[test]
fn a_note_that_names_a_place_past_the_end_of_the_file_says_nothing_of_the_log() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    let ends = grow(&mut log, "a", 3);
    drop(log);
    // the note of the log as it was, beside a file that ends before the
    // boundary it names
    let file = fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("log"))
        .unwrap();
    file.set_len(ends[1].end).unwrap();
    assert_eq!(alt_oplog::bounded_in(dir.path()), None);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 2);
    // the frames say how far the log was synced when they were written
    assert_eq!(log.bounded_to(), Some(ends[0].end));
    assert!(log.unsettled());
    assert_eq!(log.settle(|_| Ok::<_, OpLogError>(true)).unwrap(), None);
    log.append("a", 9, b"next").unwrap();
    log.sync().unwrap();
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(log.appended_len()));
}
