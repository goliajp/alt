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
    let dir = tempfile::tempdir().unwrap();
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
    assert_eq!(log.ops()[0].id, a);
    assert_eq!(log.ops()[0].parent, ROOT);
    assert_eq!(log.ops()[1].parent, a);
    assert_eq!(log.get(&b).unwrap().payload, b"second");
}

#[test]
fn torn_tail_is_truncated_and_log_keeps_accepting() {
    let dir = tempfile::tempdir().unwrap();
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
    assert_eq!(log.ops()[1].parent, a);
}

#[test]
fn corrupt_record_mid_file_refuses_to_open() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = OpLog::open(dir.path()).unwrap();
        log.append("a", 1, b"one").unwrap();
        log.append("a", 2, b"two").unwrap();
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
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
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
