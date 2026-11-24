//! `tail`, the note beside a version 2 log, when it does not read or is
//! not there: one that is being written is waited for, and one that
//! stays unreadable, is gone, or speaks of another log may have said
//! that ops are left to be looked at.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::time::Duration;

use alt_oplog::{Op, OpLog, OpLogError};

use super::v2_bounds::stage;
use super::v2_log::{boot, grow, new_log};
use super::v2_named::collecting;

fn but_left(op: &Op) -> Result<bool, OpLogError> {
    Ok(op.payload != b"left")
}

/// A log in `dir` whose two synced ops boot 1 left with an op `left`
/// after them that it did not sync, in boot 2.
fn left_by_another_boot(dir: &Path) {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    log.append("a", 3, b"left").unwrap();
    drop(log);
    boot(2);
}

/// Leaves `tail` in `dir` as no writer does: with a byte changed, cut
/// short, empty, or not there.
fn spoil(dir: &Path, how: &str) {
    let path = dir.join("tail");
    let mut noted = fs::read(&path).unwrap();
    match how {
        "a byte" => noted[41] ^= 1,
        "short" => noted.truncate(40),
        "gone" => return fs::remove_file(&path).unwrap(),
        _ => noted.clear(),
    }
    fs::write(&path, noted).unwrap();
}

const SPOILED: [&str; 4] = ["a byte", "short", "empty", "gone"];

#[test]
fn a_note_that_does_not_read_leaves_what_another_boot_left_to_be_looked_at() {
    for how in SPOILED {
        let dir = alt_os::temp::tempdir().unwrap();
        left_by_another_boot(dir.path());
        // a writer that looks at nothing notes that `left` is to be
        // looked at, and appends over blocks: a handle opened after
        // begins at a checkpoint of this boot
        let mut staging = OpLog::open(dir.path()).unwrap();
        stage(&mut staging, 40);
        drop(staging);
        assert!(OpLog::open(dir.path()).unwrap().unsettled());
        spoil(dir.path(), how);
        let reader = OpLog::open_read_only(dir.path(), None).unwrap();
        assert!(reader.unsettled(), "{how}: a reader");
        let found = reader.unconfirmed(but_left).unwrap();
        assert_eq!(found.map(|found| found.ops), Some(41), "{how}");
        let mut writer = OpLog::open(dir.path()).unwrap();
        assert!(writer.unsettled(), "{how}: a writer");
        assert_eq!(writer.settle(but_left).unwrap().unwrap().ops, 41);
        assert_eq!(writer.len(), 2);
        assert!(!OpLog::open(dir.path()).unwrap().unsettled());
    }
}

#[test]
fn a_sync_over_a_note_that_does_not_read_leaves_noted_that_ops_may_be_left_to_look_at() {
    for (deferred, how) in [
        (false, "a byte"),
        (true, "a byte"),
        (false, "gone"),
        (true, "gone"),
    ] {
        let dir = alt_os::temp::tempdir().unwrap();
        left_by_another_boot(dir.path());
        // a reader notes where the log ends in this file, and a handle
        // opened then holds the log up to there
        let reader = OpLog::open_read_only(dir.path(), None).unwrap();
        assert_eq!(reader.unconfirmed(but_left).unwrap().unwrap().ops, 1);
        let mut held = OpLog::open(dir.path()).unwrap();
        assert_eq!(held.len(), 2);
        let _given = collecting(&mut held);
        held.set_defer_durability(deferred);
        let sink = held.sync_handle().unwrap();
        let mut staging = OpLog::open(dir.path()).unwrap();
        stage(&mut staging, 40);
        drop(staging);
        spoil(dir.path(), how);
        held.sync().unwrap();
        if deferred {
            sink.fsync().unwrap();
        }
        assert_eq!(alt_oplog::bounded_in(dir.path()), Some(held.appended_len()));
        // `left` lies past where that sync took the log for synced
        let mut writer = OpLog::open(dir.path()).unwrap();
        assert!(writer.unsettled(), "deferred: {deferred}, {how}");
        assert_eq!(writer.settle(but_left).unwrap().unwrap().ops, 41);
    }
}

#[test]
fn a_note_that_is_being_written_is_read_once_its_writer_has_written_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    let synced = log.appended_len();
    drop(log);
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(synced));

    // whoever writes the note holds the lock beside it; this one has
    // written a part of it
    let lock = OpenOptions::new()
        .write(true)
        .open(dir.path().join("lock"))
        .unwrap();
    alt_os::sys::flock(&lock, alt_os::sys::LOCK_EX).unwrap();
    let path = dir.path().join("tail");
    let whole = fs::read(&path).unwrap();
    let note = OpenOptions::new().write(true).open(&path).unwrap();
    note.write_all_at(&[0xa5; 36], 36).unwrap();
    let dir_read = dir.path().to_owned();
    let read = std::thread::spawn(move || {
        boot(1);
        let bounded = alt_oplog::bounded_in(&dir_read);
        let opened = OpLog::open_read_only(&dir_read, None).unwrap();
        (bounded, opened.bounded_to())
    });
    std::thread::sleep(Duration::from_millis(300));
    note.write_all_at(&whole, 0).unwrap();
    alt_os::sys::flock(&lock, alt_os::sys::LOCK_UN).unwrap();
    assert_eq!(read.join().unwrap(), (Some(synced), Some(synced)));
}

#[test]
fn a_look_over_a_note_that_does_not_read_beside_a_log_that_was_cut_leaves_the_ops_to_look_at() {
    for how in SPOILED {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        drop(log);
        // cut once, of an append that was never finished
        let path = dir.path().join("log");
        let mut torn = fs::read(&path).unwrap();
        torn.extend_from_slice(&[1, 2, 3]);
        fs::write(&path, torn).unwrap();
        let mut log = OpLog::open(dir.path()).unwrap();
        // a ref transaction: a sync has to see to what it names
        log.append_with("a", 3, b"left", &super::v2_image::linked())
            .unwrap();
        assert_eq!(log.state().unwrap().generation, 1);
        drop(log);
        boot(2);
        let mut staging = OpLog::open(dir.path()).unwrap();
        stage(&mut staging, 40);
        drop(staging);
        spoil(dir.path(), how);
        let spoiled = fs::read(dir.path().join("tail")).ok();
        // a reader looks, and finds `left` not vouched for
        let reader = OpLog::open_read_only(dir.path(), None).unwrap();
        assert!(reader.unsettled(), "{how}");
        let found = reader.unconfirmed(but_left).unwrap();
        assert_eq!(found.map(|found| found.ops), Some(41), "{how}");
        // it has nothing to add to a note that says nothing it can read
        let noted = fs::read(dir.path().join("tail")).ok();
        assert_eq!(noted, spoiled, "{how}: a look writes the note back");
        // whoever opens the log after has `left` to look at still
        let reader = OpLog::open_read_only(dir.path(), None).unwrap();
        assert!(reader.unsettled(), "{how}: a reader after the look");
        let mut writer = OpLog::open(dir.path()).unwrap();
        assert!(writer.unsettled(), "{how}: a writer after the look");
        // and a writer that syncs gives it to whoever sees to what ops name
        let given = collecting(&mut writer);
        writer.append("a", 5, b"more").unwrap();
        writer.sync().unwrap();
        let given = given.lock().unwrap();
        assert!(given.iter().any(|op| op.payload == b"left"), "{how}");
    }
}

#[test]
fn a_note_of_a_log_of_another_generation_says_nothing_of_what_is_left_to_look_at() {
    let dir = alt_os::temp::tempdir().unwrap();
    left_by_another_boot(dir.path());
    let mut staging = OpLog::open(dir.path()).unwrap();
    stage(&mut staging, 40);
    drop(staging);
    let synced = alt_oplog::bounded_in(dir.path()).unwrap();
    let len = fs::metadata(dir.path().join("log")).unwrap().len();
    // as a writer leaves it that has looked at every op, of a log that
    // was cut five times
    let note = super::v2_note_fields::tail(2, 5, false, [synced, len, 0, 0]);
    fs::write(dir.path().join("tail"), note).unwrap();
    for read_only in [true, false] {
        let opened = match read_only {
            true => OpLog::open_read_only(dir.path(), None).unwrap(),
            false => OpLog::open(dir.path()).unwrap(),
        };
        assert!(opened.unsettled(), "read only: {read_only}");
    }
}

#[test]
fn reading_a_note_that_does_not_read_makes_no_lock_beside_the_log() {
    for kept in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        left_by_another_boot(dir.path());
        if kept {
            // a log this build reads and does not append to
            let bytes = fs::read(dir.path().join("log")).unwrap();
            let header = alt_oplog::v2::block::FileHeader {
                read_compatible: 1,
                ..alt_oplog::v2::block::FileHeader::decode(&bytes).unwrap()
            };
            super::v2_features::with_header(dir.path(), header);
        }
        spoil(dir.path(), "a byte");
        let lock = dir.path().join("lock");
        fs::remove_file(&lock).unwrap();
        let reader = OpLog::open_read_only(dir.path(), None).unwrap();
        assert!(reader.unsettled());
        if kept {
            // a look notes nothing beside such a log
            assert_eq!(reader.unconfirmed(but_left).unwrap().unwrap().ops, 1);
        }
        let mut opened = OpLog::open(dir.path()).unwrap();
        opened.follow().unwrap();
        assert!(alt_oplog::bounded_in(dir.path()).is_none());
        assert!(!lock.exists(), "kept from appending: {kept}");
    }
}
