//! `tail`, the note beside a version 2 log: who writes what into it, and
//! that a reader, which holds no lock on the log, writes nothing there
//! that says more than it knows.

use std::fs;
use std::path::Path;

use alt_oplog::{Meta, Op, OpLog, OpLogError};

use super::v2_fields::tail;
use super::v2_log::{boot, file_len, grow, new_log};

fn noted(dir: &Path) -> Vec<u8> {
    fs::read(dir.join("tail")).unwrap()
}

fn vouched(_: &Op) -> Result<bool, OpLogError> {
    Ok(true)
}

fn but_left(op: &Op) -> Result<bool, OpLogError> {
    Ok(op.payload != b"left")
}

/// In boot 1: two synced ops and one, `left`, that is not. Handles opened
/// after it run in boot 2.
fn left_by_boot_1(dir: &Path, meta: &Meta) -> u64 {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    let synced = log.appended_len();
    log.append_with("a", 3, b"left", meta).unwrap();
    drop(log);
    boot(2);
    synced
}

#[test]
fn a_reader_that_notes_its_look_late_hides_nothing_a_writer_synced_meanwhile() {
    for vouches in [true, false] {
        let dir = alt_os::temp::tempdir().unwrap();
        left_by_boot_1(dir.path(), &Meta::default());
        let reader = OpLog::open_read_only(dir.path(), None).unwrap();
        let mut writer = OpLog::open(dir.path()).unwrap();
        assert_eq!(writer.settle(vouched).unwrap(), None);
        let committed = writer.append("w", 5, b"committed").unwrap();
        writer.sync().unwrap();
        let synced = writer.appended_len();
        let before = noted(dir.path());

        // the reader looks only now, at the log as it was when it opened
        let found = match vouches {
            true => reader.unconfirmed(vouched).unwrap(),
            false => reader.unconfirmed(but_left).unwrap(),
        };
        assert_eq!(found.is_some(), !vouches);
        assert_eq!(noted(dir.path()), before, "what the writer noted stands");
        for read_only in [true, false] {
            let log = match read_only {
                true => OpLog::open_read_only(dir.path(), None).unwrap(),
                false => OpLog::open(dir.path()).unwrap(),
            };
            assert_eq!((log.len(), log.head()), (4, Some(committed)));
            assert_eq!(log.appended_len(), synced);
            assert_eq!(log.bounded_to(), Some(synced));
            assert!(!log.passed_over() && !log.unsettled() && log.confirmed());
        }
        // nor is a writer held to look again at what is synced
        let mut next = OpLog::open(dir.path()).unwrap();
        assert_eq!(next.settle(but_left).unwrap(), None);
        next.append("w", 6, b"next").unwrap();
        next.sync().unwrap();
    }
}

#[test]
fn a_reader_notes_nothing_of_a_log_that_was_cut_or_judged_otherwise_since_it_opened() {
    // cut meanwhile
    let dir = alt_os::temp::tempdir().unwrap();
    left_by_boot_1(dir.path(), &Meta::default());
    let reader = OpLog::open_read_only(dir.path(), None).unwrap();
    let mut writer = OpLog::open(dir.path()).unwrap();
    assert!(writer.settle(but_left).unwrap().is_some());
    let before = noted(dir.path());
    assert_eq!(reader.unconfirmed(vouched).unwrap(), None);
    assert_eq!(noted(dir.path()), before);

    // another reader noted ops it would not vouch for
    let dir = alt_os::temp::tempdir().unwrap();
    left_by_boot_1(dir.path(), &Meta::default());
    let (first, second) = (
        OpLog::open_read_only(dir.path(), None).unwrap(),
        OpLog::open_read_only(dir.path(), None).unwrap(),
    );
    assert!(first.unconfirmed(but_left).unwrap().is_some());
    let before = noted(dir.path());
    assert_eq!(second.unconfirmed(vouched).unwrap(), None);
    assert_eq!(noted(dir.path()), before);
    assert_eq!(OpLog::open_read_only(dir.path(), None).unwrap().len(), 2);
    // a writer's look replaces it
    let mut writer = OpLog::open(dir.path()).unwrap();
    assert!(writer.unsettled());
    assert_eq!(writer.settle(vouched).unwrap(), None);
    let log = OpLog::open_read_only(dir.path(), None).unwrap();
    assert!(log.len() == 3 && !log.unsettled());

    // a reader that vouched: one that would not, and opened before, leaves it
    let dir = alt_os::temp::tempdir().unwrap();
    left_by_boot_1(dir.path(), &Meta::default());
    let (first, second) = (
        OpLog::open_read_only(dir.path(), None).unwrap(),
        OpLog::open_read_only(dir.path(), None).unwrap(),
    );
    assert_eq!(first.unconfirmed(vouched).unwrap(), None);
    let before = noted(dir.path());
    assert!(second.unconfirmed(but_left).unwrap().is_some());
    assert_eq!(noted(dir.path()), before);
    assert_eq!(OpLog::open_read_only(dir.path(), None).unwrap().len(), 3);
}

#[test]
fn what_a_reader_would_not_vouch_for_ends_the_log_in_a_file_as_long_as_the_one_it_read() {
    let dir = alt_os::temp::tempdir().unwrap();
    left_by_boot_1(dir.path(), &Meta::default());
    let reader = OpLog::open_read_only(dir.path(), None).unwrap();
    let read = file_len(dir.path());
    // the file grows after the reader read it: an append that is not finished
    let mut bytes = fs::read(dir.path().join("log")).unwrap();
    bytes.extend_from_slice(&[1, 2, 3]);
    fs::write(dir.path().join("log"), &bytes).unwrap();
    assert!(reader.unconfirmed(but_left).unwrap().is_some());
    // of that file the reader said nothing: it is read as far as it reads
    let log = OpLog::open_read_only(dir.path(), None).unwrap();
    assert_eq!((log.len(), log.appended_len()), (3, read));
    assert!(log.unsettled(), "the op is still left to be looked at");
    // as long as the reader read it, it ends before the op
    fs::write(dir.path().join("log"), &bytes[..read as usize]).unwrap();
    let log = OpLog::open_read_only(dir.path(), None).unwrap();
    assert!(log.len() == 2 && log.passed_over() && log.unsettled());
}

#[test]
fn a_note_that_ops_are_left_from_before_the_boundary_has_those_past_it_looked_at() {
    let dir = alt_os::temp::tempdir().unwrap();
    let synced = left_by_boot_1(dir.path(), &Meta::default());
    // as no writer leaves it: ops to look at from the first frame on
    let note = tail(2, 0, true, [synced, 64, 0]);
    fs::write(dir.path().join("tail"), note).unwrap();
    let mut asked = Vec::new();
    let mut log = OpLog::open(dir.path()).unwrap();
    let cut = log.settle(|op| {
        asked.push(op.payload.clone());
        Ok::<_, OpLogError>(false)
    });
    assert_eq!(cut.unwrap().unwrap().at, synced);
    assert_eq!(asked, [b"left"]);
    assert_eq!(log.len(), 2);
}
