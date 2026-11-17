//! Lifecycle: reopen replay, torn-tail recovery, corruption and chain
//! tampering detection.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use alt_oplog::{OpLog, OpLogError, ROOT};

fn log_path(dir: &Path) -> std::path::PathBuf {
    dir.join("log")
}

#[test]
fn reopen_replays_the_chain() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (a, b) = {
        let mut log = OpLog::open(dir.path()).unwrap();
        let a = log.append("alice", 1, b"first").unwrap();
        let b = log.append("bob", 2, b"second").unwrap();
        log.sync().unwrap();
        (a, b)
    };

    let log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log.head(), Some(b));
    assert_eq!(log.ops().unwrap()[0].id, a);
    assert_eq!(log.ops().unwrap()[0].parent, ROOT);
    assert_eq!(log.ops().unwrap()[1].parent, a);
    assert_eq!(log.get(&b).unwrap().unwrap().payload, b"second");
}

#[test]
fn torn_tail_is_truncated_and_log_keeps_accepting() {
    let dir = alt_os::temp::tempdir().unwrap();
    let a = {
        let mut log = OpLog::open(dir.path()).unwrap();
        let a = log.append("alice", 1, b"durable").unwrap();
        log.sync().unwrap();
        a
    };

    // crash mid-append: a partial record after the durable one
    let mut f = OpenOptions::new()
        .append(true)
        .open(log_path(dir.path()))
        .unwrap();
    f.write_all(&[0x55; 17]).unwrap();
    drop(f);

    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log.head(), Some(a));
    let b = log.append("bob", 2, b"after recovery").unwrap();
    drop(log);

    let log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.head(), Some(b));
    assert_eq!(log.ops().unwrap()[1].parent, a);
}

#[test]
fn corrupt_record_mid_file_refuses_to_open() {
    let dir = alt_os::temp::tempdir().unwrap();
    {
        let mut log = OpLog::open(dir.path()).unwrap();
        log.append("a", 1, b"one").unwrap();
        log.append("a", 2, b"two").unwrap();
        log.sync().unwrap();
    }

    let path = log_path(dir.path());
    let mut bytes = fs::read(&path).unwrap();
    bytes[5 + 8] ^= 0xFF; // inside the first record's body
    fs::write(&path, &bytes).unwrap();

    let err = match OpLog::open(dir.path()) {
        Ok(_) => panic!("corrupt oplog must refuse to open"),
        Err(e) => e,
    };
    assert!(matches!(err, OpLogError::Format(_)), "got {err:?}");
}

#[test]
fn spliced_foreign_record_breaks_the_chain() {
    let dir_a = alt_os::temp::tempdir().unwrap();
    let dir_b = alt_os::temp::tempdir().unwrap();
    {
        let mut log = OpLog::open(dir_a.path()).unwrap();
        log.append("a", 1, b"a1").unwrap();
        log.append("a", 2, b"a2").unwrap();
    }
    {
        // a different first op: internally valid, but its parent is ROOT,
        // which does not match log A's head
        let mut log = OpLog::open(dir_b.path()).unwrap();
        log.append("b", 9, b"b1").unwrap();
    }

    let mut a_bytes = fs::read(log_path(dir_a.path())).unwrap();
    let b_bytes = fs::read(log_path(dir_b.path())).unwrap();
    a_bytes.extend_from_slice(&b_bytes[5..]); // splice B's record onto A
    fs::write(log_path(dir_a.path()), &a_bytes).unwrap();

    let err = match OpLog::open(dir_a.path()) {
        Ok(_) => panic!("a broken chain must refuse to open"),
        Err(e) => e,
    };
    assert!(matches!(err, OpLogError::Format(_)), "got {err:?}");
}

fn grow(dir: &Path, range: std::ops::Range<u32>) {
    let mut log = OpLog::open(dir).unwrap();
    for n in range {
        log.append("writer", n as u64, format!("op {n}").as_bytes())
            .unwrap();
    }
    log.sync().unwrap();
}

#[test]
fn open_at_an_anchor_replays_only_what_follows_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    grow(dir.path(), 0..10);
    let anchor = OpLog::open(dir.path()).unwrap().anchor().unwrap();
    assert_eq!(anchor.count, 10);
    grow(dir.path(), 10..13);

    let mut log = OpLog::open_at(dir.path(), Some(anchor)).unwrap();
    assert!(log.anchored());
    assert_eq!(log.len(), 13);
    assert_eq!(log.recent().len(), 3);
    assert_eq!(log.recent()[0].parent, anchor.op);
    assert_eq!(log.recent()[0].payload, b"op 10");

    // the head anchor of an anchored log is usable in turn
    let head = log.anchor().unwrap();
    assert_eq!(head.count, 13);
    let at_head = OpLog::open_at(dir.path(), Some(head)).unwrap();
    assert!(at_head.anchored());
    assert!(at_head.recent().is_empty());
    assert_eq!(at_head.head(), Some(head.op));
    assert_eq!(at_head.anchor(), Some(head));

    // appends chain onto the true head and show up in the full history,
    // whether it was read before or after them
    let next = log.append("writer", 13, b"op 13").unwrap();
    assert_eq!(log.recent().last().unwrap().parent, head.op);
    let all = log.ops().unwrap();
    assert_eq!(all.len(), 14);
    assert_eq!(all[0].parent, ROOT);
    assert_eq!(all[9].id, anchor.op);
    assert_eq!(all[13].id, next);
    let after = log.append("writer", 14, b"op 14").unwrap();
    assert_eq!(log.ops().unwrap().last().unwrap().id, after);
    assert_eq!(log.get(&anchor.op).unwrap().unwrap().payload, b"op 9");
    assert_eq!(
        OpLog::open(dir.path()).unwrap().ops().unwrap(),
        log.ops().unwrap()
    );
}

#[test]
fn an_anchor_the_log_no_longer_holds_falls_back_to_a_full_replay() {
    let dir = alt_os::temp::tempdir().unwrap();
    grow(dir.path(), 0..5);
    let anchor = OpLog::open(dir.path()).unwrap().anchor().unwrap();

    // a different log of the same shape: the record ending there is another op
    let other = alt_os::temp::tempdir().unwrap();
    grow(other.path(), 100..105);
    let log = OpLog::open_at(other.path(), Some(anchor)).unwrap();
    assert!(!log.anchored());
    assert_eq!(log.recent().len(), 5);

    // a log cut back before the anchor
    let path = log_path(dir.path());
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(anchor.start)
        .unwrap();
    let log = OpLog::open_at(dir.path(), Some(anchor)).unwrap();
    assert!(!log.anchored());
    assert_eq!(log.len(), 4);
}

#[test]
fn corruption_before_an_anchor_surfaces_when_the_history_is_read() {
    let dir = alt_os::temp::tempdir().unwrap();
    grow(dir.path(), 0..6);
    let anchor = OpLog::open(dir.path()).unwrap().anchor().unwrap();
    grow(dir.path(), 6..8);
    let path = log_path(dir.path());
    let mut bytes = fs::read(&path).unwrap();
    bytes[5 + 4 + 70] ^= 1; // inside the first record's body
    fs::write(&path, &bytes).unwrap();

    let log = OpLog::open_at(dir.path(), Some(anchor)).unwrap();
    assert_eq!(log.recent().len(), 2, "what follows the anchor is intact");
    assert!(matches!(log.ops(), Err(OpLogError::Format(_))));
    assert!(matches!(
        OpLog::open(dir.path()),
        Err(OpLogError::Format(_))
    ));
}

#[test]
fn a_torn_tail_after_an_anchor_is_passed_over_and_cut_off_by_the_next_append() {
    let dir = alt_os::temp::tempdir().unwrap();
    grow(dir.path(), 0..4);
    let anchor = OpLog::open(dir.path()).unwrap().anchor().unwrap();
    // two appends that were never synced, the second cut short
    let mut log = OpLog::open(dir.path()).unwrap();
    log.append("writer", 4, b"op 4").unwrap();
    log.append("writer", 5, b"op 5").unwrap();
    std::mem::forget(log);
    let path = log_path(dir.path());
    let size = fs::metadata(&path).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(size - 3)
        .unwrap();

    let mut log = OpLog::open_at(dir.path(), Some(anchor)).unwrap();
    assert!(log.anchored());
    assert_eq!(log.len(), 5);
    assert_eq!(fs::metadata(&path).unwrap().len(), size - 3);
    assert!(log.anchor().unwrap().end < size - 3);
    log.append("writer", 9, b"after the cut").unwrap();
    let end = log.anchor().unwrap().end;
    assert_eq!(fs::metadata(&path).unwrap().len(), end);
    assert_eq!(OpLog::open(dir.path()).unwrap().len(), 6);
}
