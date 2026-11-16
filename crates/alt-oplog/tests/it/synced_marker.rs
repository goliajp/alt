//! The synced length in `durable` decides what an open does with a record
//! it cannot read: damage in what was synced is reported and the log left
//! as it is; appends that never finished, past it, are cut off.

use std::fs::{self, OpenOptions};
use std::io::Write;
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
    assert_eq!(fs::metadata(&path).unwrap().len(), whole);
    log.append("someone", 10, b"after the cut").unwrap();
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
fn nothing_is_cut_when_the_marker_is_missing_or_an_older_one() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (_, whole) = ten(dir.path());
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&[0xcd; 30]).unwrap();
    let marker = dir.path().join("durable");
    // the file length an older alt records, and nothing else
    fs::write(&marker, whole.to_le_bytes()).unwrap();
    assert!(corrupt(OpLog::open(dir.path())));
    fs::remove_file(&marker).unwrap();
    assert!(corrupt(OpLog::open(dir.path())));
    assert_eq!(fs::metadata(&path).unwrap().len(), whole + 30);
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
