//! The synced length in `durable` decides what an open does with a record
//! it cannot read: damage in what was synced is reported and the log left
//! as it is; appends that never finished, past it, are passed over, and
//! cut off by the next append.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_oplog::{OpLog, OpLogError};

/// Ten synced ops; where each record starts, and the log's length.
fn ten(dir: &Path) -> (Vec<u64>, u64) {
    let path = dir.join("log");
    let mut log = OpLog::open(dir).unwrap();
    let mut starts = Vec::new();
    for n in 0..10u64 {
        starts.push(fs::metadata(&path).unwrap().len());
        log.append("someone", n, format!("payload {n}").as_bytes())
            .unwrap();
    }
    log.sync().unwrap();
    (starts, fs::metadata(&path).unwrap().len())
}

fn write_at(path: &Path, at: u64, bytes: &[u8]) {
    let file = OpenOptions::new().write(true).open(path).unwrap();
    file.write_all_at(bytes, at).unwrap();
}

fn corrupt(opened: Result<OpLog, OpLogError>) -> bool {
    matches!(opened, Err(OpLogError::Format("oplog record corrupt")))
}

#[test]
fn a_damaged_record_in_the_synced_log_is_reported_and_nothing_is_cut() {
    // the length of a record in the middle made larger, so that it runs
    // past the end like an unfinished one; then the last record's body
    for (record, offset) in [(3, 2), (9, 20)] {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("log");
        let (starts, whole) = ten(dir.path());
        write_at(&path, starts[record] + offset, &[0x01]);
        let before = fs::read(&path).unwrap();
        assert!(corrupt(OpLog::open(dir.path())), "record {record}");
        assert!(corrupt(OpLog::open_read_only(dir.path(), None)));
        assert!(fs::read(&path).unwrap() == before);
        assert_eq!(before.len() as u64, whole);
    }
}

#[test]
fn appends_that_never_finished_are_cut_off_however_many() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (_, whole) = ten(dir.path());
    let mut log = OpLog::open(dir.path()).unwrap();
    for n in 10..13u64 {
        log.append("someone", n, b"never synced").unwrap();
    }
    std::mem::forget(log);
    let unsynced = fs::metadata(&path).unwrap().len();
    // power went out: the first of the three did not reach the disk
    write_at(&path, whole + 8, &[0u8; 40]);

    let read = OpLog::open_read_only(dir.path(), None).unwrap();
    assert_eq!(read.len(), 10);
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        unsynced,
        "a read-only open cuts nothing"
    );
    drop(read);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 10);
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        unsynced,
        "nor does any other open"
    );
    log.append("someone", 10, b"after the cut").unwrap();
    assert!(fs::metadata(&path).unwrap().len() < unsynced);
    assert_eq!(OpLog::open(dir.path()).unwrap().len(), 11);
}

#[test]
fn zeros_after_the_synced_log_are_no_record_and_are_cut_by_the_next_append() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (_, whole) = ten(dir.path());
    // a record of no bytes with a zero checksum, a hundred times over
    write_at(&path, whole, &[0u8; 1200]);
    assert_eq!(OpLog::open_read_only(dir.path(), None).unwrap().len(), 10);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 10);
    assert_eq!(fs::metadata(&path).unwrap().len(), whole + 1200);
    let before = fs::read(&path).unwrap();
    log.append("someone", 10, b"after the zeros").unwrap();
    let after = fs::read(&path).unwrap();
    assert_eq!(after[..whole as usize], before[..whole as usize]);
    assert!(after.len() < before.len() && after[whole as usize..].iter().any(|b| *b != 0));
    assert_eq!(OpLog::open(dir.path()).unwrap().len(), 11);
}

#[test]
fn a_writer_catching_up_reports_damage_in_what_another_synced() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let mut held = OpLog::open(dir.path()).unwrap();
    let (starts, whole) = ten(dir.path());
    write_at(&path, starts[9] + 20, &[0x01]);
    assert!(held.append("someone", 99, b"mine").is_err());
    assert_eq!(fs::metadata(&path).unwrap().len(), whole);
    assert_eq!(held.len(), 0, "the handle is where it was");
}

#[test]
fn a_sink_records_how_far_a_deferred_sync_wrote() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let mut log = OpLog::open(dir.path()).unwrap();
    log.set_defer_durability(true);
    let sink = log.sync_handle().unwrap();
    log.append("someone", 1, b"one").unwrap();
    log.append("someone", 2, b"two").unwrap();
    log.sync().unwrap();
    sink.fsync().unwrap();
    drop(log);
    // the last record: only the marker tells it from an unfinished append
    let whole = fs::metadata(&path).unwrap().len();
    write_at(&path, whole - 12, &[0xff]);
    assert!(corrupt(OpLog::open(dir.path())));
    assert_eq!(fs::metadata(&path).unwrap().len(), whole);
}
