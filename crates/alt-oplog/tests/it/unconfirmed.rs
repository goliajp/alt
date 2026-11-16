//! Ops past the synced boundary that name content which is not stored: a
//! writer cuts the log from the first of them on, a reader is told where
//! it starts, and a handle that held what was cut finds out.

use std::fs;
use std::path::Path;

use alt_oplog::{Op, OpLog, OpLogError, Unconfirmed};

/// Three synced ops, then `unsynced` appended and left so. Returns where
/// each record ends.
fn log_with(dir: &Path, unsynced: &[&[u8]]) -> Vec<u64> {
    let mut log = OpLog::open(dir).unwrap();
    let mut ends = Vec::new();
    for n in 0..3u64 {
        log.append("actor", n, b"synced").unwrap();
        log.sync().unwrap();
        ends.push(log.appended_len());
    }
    for payload in unsynced {
        log.append("actor", 9, payload).unwrap();
        ends.push(log.appended_len());
    }
    ends
}

fn held(op: &Op) -> Result<bool, OpLogError> {
    Ok(!op.payload.starts_with(b"gone"))
}

fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut all: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read(path).unwrap())
        })
        .collect();
    all.sort();
    all
}

#[test]
fn a_writer_cuts_from_the_first_unsynced_op_that_names_what_is_not_stored() {
    let dir = alt_os::temp::tempdir().unwrap();
    let ends = log_with(dir.path(), &[b"kept", b"gone", b"after it"]);
    let mut log = OpLog::open(dir.path()).unwrap();
    let cut = log.settle(held).unwrap();
    assert_eq!(
        cut,
        Some(Unconfirmed {
            at: ends[3],
            ops: 2
        })
    );
    assert_eq!(fs::metadata(dir.path().join("log")).unwrap().len(), ends[3]);
    assert_eq!(log.len(), 4);
    assert_eq!(log.recent().last().unwrap().payload, b"kept");
    // nothing more to cut, and the log takes appends
    assert_eq!(log.settle(held).unwrap(), None);
    log.append("actor", 10, b"next").unwrap();
    log.sync().unwrap();
    drop(log);
    let log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 5);
    assert!(log.confirmed());
}

#[test]
fn an_op_before_the_boundary_is_not_cut() {
    let dir = alt_os::temp::tempdir().unwrap();
    log_with(dir.path(), &[]);
    let before = files(dir.path());
    let mut log = OpLog::open(dir.path()).unwrap();
    let none = |_: &Op| Ok::<_, OpLogError>(false);
    assert_eq!(log.settle(none).unwrap(), None);
    assert_eq!(log.unconfirmed(none).unwrap(), None);
    assert_eq!(files(dir.path()), before);
}

#[test]
fn nothing_is_cut_where_the_marker_records_no_boundary() {
    for broken in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        log_with(dir.path(), &[b"gone"]);
        let marker = dir.path().join("durable");
        if broken {
            fs::write(&marker, [0u8; 3]).unwrap();
        } else {
            fs::remove_file(&marker).unwrap();
        }
        let before = files(dir.path());
        let mut log = OpLog::open(dir.path()).unwrap();
        assert_eq!(log.unconfirmed(held).unwrap(), None);
        assert_eq!(log.settle(held).unwrap(), None);
        assert_eq!(files(dir.path()), before, "marker broken: {broken}");
    }
}

#[test]
fn ops_past_the_length_an_older_alt_recorded_are_cut_and_the_marker_replaced() {
    let dir = alt_os::temp::tempdir().unwrap();
    let ends = log_with(dir.path(), &[b"gone"]);
    fs::write(dir.path().join("durable"), ends[2].to_le_bytes()).unwrap();
    let mut log = OpLog::open(dir.path()).unwrap();
    let cut = log.settle(held).unwrap();
    assert_eq!(
        cut,
        Some(Unconfirmed {
            at: ends[2],
            ops: 1
        })
    );
    assert_eq!(fs::metadata(dir.path().join("log")).unwrap().len(), ends[2]);
    assert!(log.confirmed(), "a marker that records the boundary");
    log.append("actor", 10, b"next").unwrap();
    log.sync().unwrap();
    assert_eq!(OpLog::open(dir.path()).unwrap().len(), 4);
}

#[test]
fn a_reader_is_told_where_the_ops_start_and_changes_nothing() {
    let dir = alt_os::temp::tempdir().unwrap();
    let ends = log_with(dir.path(), &[b"gone", b"after it"]);
    let before = files(dir.path());
    let log = OpLog::open_read_only(dir.path(), None).unwrap();
    assert!(!log.confirmed());
    let found = log.unconfirmed(held).unwrap();
    assert_eq!(
        found,
        Some(Unconfirmed {
            at: ends[2],
            ops: 2
        })
    );
    // the log as it stood before them
    alt_oplog::end_reads_at(dir.path(), ends[2]).unwrap();
    let log = OpLog::open_read_only(dir.path(), None).unwrap();
    assert_eq!(log.len(), 3);
    assert!(log.clamped());
    assert_eq!(files(dir.path()), before);
    // the process reads this file short for as long as it runs: kept open,
    // its inode is not given to the log of another test
    std::mem::forget(log);
}

#[test]
fn a_handle_whose_op_was_cut_is_refused_until_it_reads_the_log_again() {
    let dir = alt_os::temp::tempdir().unwrap();
    log_with(dir.path(), &[]);
    let mut victim = OpLog::open(dir.path()).unwrap();
    victim.append("victim", 5, b"gone").unwrap();

    let mut other = OpLog::open(dir.path()).unwrap();
    assert!(other.settle(held).unwrap().is_some());
    // as long as what was cut: the length alone does not tell
    other.append("victim", 5, b"here").unwrap();
    other.sync().unwrap();

    assert!(matches!(victim.sync(), Err(OpLogError::CutBack)));
    assert!(matches!(
        victim.append("victim", 6, b"more"),
        Err(OpLogError::CutBack)
    ));
    assert!(matches!(victim.refresh(), Err(OpLogError::CutBack)));
    victim.reopen().unwrap();
    assert_eq!(victim.reads(), 1);
    assert_eq!(victim.recent().last().unwrap().payload, b"here");
    victim.append("victim", 6, b"more").unwrap();
    victim.sync().unwrap();
    let log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 5);
    assert!(log.confirmed());
}

#[test]
fn a_handle_whose_op_stayed_syncs_it_after_a_cut_behind_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    log_with(dir.path(), &[]);
    let mut kept = OpLog::open(dir.path()).unwrap();
    kept.append("kept", 5, b"kept").unwrap();
    let end = kept.appended_len();
    let mut dead = OpLog::open(dir.path()).unwrap();
    dead.append("dead", 6, b"gone").unwrap();
    drop(dead);

    let mut other = OpLog::open(dir.path()).unwrap();
    let cut = other.settle(held).unwrap();
    assert_eq!(cut, Some(Unconfirmed { at: end, ops: 1 }));
    assert!(!other.confirmed());
    kept.sync().unwrap();
    assert!(OpLog::open(dir.path()).unwrap().confirmed());
}
