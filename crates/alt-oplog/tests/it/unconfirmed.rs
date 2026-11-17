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

/// A writer gives up the lock to have what the ops of a log without a
/// boundary name synced. Ops it held that another writer cuts meanwhile
/// are told of like any cut: nothing is appended at an offset that is no
/// longer the end of what the handle read.
#[test]
fn a_cut_made_while_what_the_ops_name_is_synced_refuses_the_append() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let dir = alt_os::temp::tempdir().unwrap();
    let ends = log_with(dir.path(), &[b"gone"]);
    // as an older alt leaves it: the file length at its last fsync
    fs::write(dir.path().join("durable"), ends[2].to_le_bytes()).unwrap();

    let mut victim = OpLog::open(dir.path()).unwrap();
    assert_eq!(victim.len(), 4);
    let (cut, path) = (Arc::new(AtomicBool::new(false)), dir.path().to_owned());
    let done = cut.clone();
    victim.sync_named_with(Arc::new(move || {
        if !done.swap(true, Ordering::Relaxed) {
            let mut other = OpLog::open(&path).unwrap();
            assert!(other.settle(held).unwrap().is_some());
            other.append("actor", 5, b"here").unwrap();
            other.sync().unwrap();
        }
        Ok(())
    }));
    let after_cut = |dir: &Path| fs::read(dir.join("log")).unwrap();
    assert!(matches!(
        victim.append("actor", 6, b"more"),
        Err(OpLogError::CutBack)
    ));
    assert!(cut.load(Ordering::Relaxed));
    let log = after_cut(dir.path());
    assert_eq!(victim.len(), 4, "the handle stays where it was");
    victim.reopen().unwrap();
    assert_eq!(victim.recent().last().unwrap().payload, b"here");
    victim.append("actor", 6, b"more").unwrap();
    victim.sync().unwrap();
    assert!(after_cut(dir.path()).starts_with(&log));
    let log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 5);
    assert!(log.confirmed());
}

fn flip(dir: &Path, at: u64) {
    let path = dir.join("log");
    let mut bytes = fs::read(&path).unwrap();
    bytes[at as usize] ^= 0xff;
    fs::write(path, bytes).unwrap();
}

fn payloads(ops: &[Op]) -> Vec<&[u8]> {
    ops.iter().map(|op| &op.payload[..]).collect()
}

#[test]
fn what_a_writer_cuts_is_kept_with_the_ops_among_it_that_read() {
    let dir = alt_os::temp::tempdir().unwrap();
    let unsynced: [&[u8]; 6] = [
        b"kept",
        b"gone",
        b"after it",
        b"unread",
        b"past it",
        b"last",
    ];
    let ends = log_with(dir.path(), &unsynced);
    // the record of `unread`: the log ends before it for every reader
    flip(dir.path(), ends[5] + 20);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 6);
    assert_eq!(log.unsynced_from(), Some(ends[2]));
    let passed = log.passed_ops().unwrap();
    assert_eq!(payloads(&passed), [&b"past it"[..], b"last"]);
    assert!(log.take_discarded().is_empty());

    let cut = log.settle(held).unwrap().unwrap();
    assert_eq!((cut.at, cut.ops), (ends[3], 2));
    let discarded = log.take_discarded();
    assert_eq!(discarded.len(), 2);
    // what follows the record that does not read goes first
    let (tail, named) = (&discarded[0], &discarded[1]);
    assert_eq!((tail.from, tail.to), (ends[5], ends[8]));
    assert_eq!(tail.unread, ends[6] - ends[5]);
    assert_eq!(payloads(&tail.ops), [&b"past it"[..], b"last"]);
    assert_eq!((named.from, named.to, named.unread), (ends[3], ends[5], 0));
    assert_eq!(payloads(&named.ops), [&b"gone"[..], b"after it"]);
    assert!(log.take_discarded().is_empty());
    assert_eq!(fs::metadata(dir.path().join("log")).unwrap().len(), ends[3]);
}

#[test]
fn an_append_keeps_word_of_the_end_it_cut_off_first() {
    let dir = alt_os::temp::tempdir().unwrap();
    let ends = log_with(dir.path(), &[b"unread", b"past it"]);
    flip(dir.path(), ends[2] + 20);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 3);
    log.append("actor", 10, b"next").unwrap();
    let discarded = log.take_discarded();
    assert_eq!(discarded.len(), 1);
    assert_eq!((discarded[0].from, discarded[0].to), (ends[2], ends[4]));
    assert_eq!(payloads(&discarded[0].ops), [&b"past it"[..]]);
    assert_eq!(discarded[0].unread, ends[3] - ends[2]);
    // an append that never finished holds no op
    drop(log);
    let mut all = fs::read(dir.path().join("log")).unwrap();
    all.extend_from_slice(&[0xcd; 9]);
    fs::write(dir.path().join("log"), all).unwrap();
    let mut log = OpLog::open(dir.path()).unwrap();
    log.append("actor", 11, b"one more").unwrap();
    let discarded = log.take_discarded();
    assert_eq!(discarded.len(), 1);
    assert!(discarded[0].ops.is_empty());
    assert_eq!(discarded[0].unread, 9);
}
