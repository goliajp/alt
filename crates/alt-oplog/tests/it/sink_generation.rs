//! A handle that leaves its fsyncs to a sink, after records were cut off
//! the log by another writer: the marker is at another generation than
//! the handle appended under. Where the handle's record is still in its
//! place the sink records it as synced, as a handle that syncs inline
//! does; where it was cut, the sink says so.

use std::fs;
use std::path::Path;

use alt_oplog::{OpLog, OpLogError};

use super::marker::recorded;

fn len(dir: &Path) -> u64 {
    fs::metadata(dir.join("log")).unwrap().len()
}

/// One synced op, then one whose sync is left to the sink of the handle
/// returned, then one of another writer that was never synced.
fn deferred_then_another(dir: &Path) -> (OpLog, u64) {
    let mut log = OpLog::open(dir).unwrap();
    log.append("a", 1, b"synced").unwrap();
    log.sync().unwrap();
    log.set_defer_durability(true);
    log.append("a", 2, b"deferred").unwrap();
    log.sync().unwrap();
    let end = len(dir);
    let mut other = OpLog::open(dir).unwrap();
    other.append("c", 3, b"gone").unwrap();
    std::mem::forget(other);
    (log, end)
}

fn cut_where(dir: &Path, gone: &[&[u8]]) {
    let mut writer = OpLog::open(dir).unwrap();
    let held = |op: &alt_oplog::Op| Ok::<_, OpLogError>(!gone.contains(&&op.payload[..]));
    assert!(writer.settle(held).unwrap().is_some());
    assert_eq!(recorded(dir).flags >> 8, 1);
}

#[test]
fn a_sink_records_its_append_as_synced_under_the_generation_a_cut_after_it_left() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (mut log, end) = deferred_then_another(dir.path());
    let sink = log.sync_handle().unwrap();
    cut_where(dir.path(), &[b"gone"]);
    assert_eq!(len(dir.path()), end);
    assert!(recorded(dir.path()).whole < end);
    sink.fsync().unwrap();
    let marker = recorded(dir.path());
    assert_eq!((marker.flags >> 8, marker.whole), (1, end));
    // the handle goes on, and its sink with it
    log.append("a", 4, b"next").unwrap();
    log.sync().unwrap();
    sink.fsync().unwrap();
    assert_eq!(recorded(dir.path()).whole, len(dir.path()));
    assert_eq!(OpLog::open(dir.path()).unwrap().len(), 3);
}

#[test]
fn a_sink_whose_append_was_cut_says_so() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (log, end) = deferred_then_another(dir.path());
    let sink = log.sync_handle().unwrap();
    cut_where(dir.path(), &[b"deferred", b"gone"]);
    assert!(len(dir.path()) < end);
    assert!(matches!(sink.fsync(), Err(OpLogError::CutBack)));
    assert!(recorded(dir.path()).whole < end);
}
