//! A panic in what a caller has run under the log's lock.

use std::fs::File;
use std::panic::{AssertUnwindSafe, catch_unwind};

use alt_oplog::{OpLog, OpLogError};
use alt_os::sys::{LOCK_EX, LOCK_NB, flock};

#[test]
fn a_panic_in_the_check_of_an_append_gives_the_lock_up() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = OpLog::open(dir.path()).unwrap();
    log.append("a", 1, b"first").unwrap();
    let appending = AssertUnwindSafe(|| {
        let _ = log.append_checked::<_, OpLogError>("a", 2, |_| panic!("in the check"));
    });
    assert!(catch_unwind(appending).is_err());
    // the handle lives on, as in a process that goes on after the panic
    let other = File::open(dir.path().join("log")).unwrap();
    flock(&other, LOCK_EX | LOCK_NB).unwrap();
    drop(other);
    log.append("a", 3, b"second").unwrap();
    assert_eq!(log.len(), 2);
}

#[test]
fn a_panic_in_what_vouches_for_an_op_gives_the_lock_up() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = OpLog::open(dir.path()).unwrap();
    log.append("a", 1, b"first").unwrap();
    log.sync().unwrap();
    // past the boundary: a writer that settles the log looks at it
    log.append("a", 2, b"second").unwrap();
    drop(log);
    let mut log = OpLog::open(dir.path()).unwrap();
    let settling = AssertUnwindSafe(|| {
        let _ = log.settle::<OpLogError>(|_| panic!("in the check"));
    });
    assert!(catch_unwind(settling).is_err());
    let other = File::open(dir.path().join("log")).unwrap();
    flock(&other, LOCK_EX | LOCK_NB).unwrap();
    drop(other);
    log.append("a", 3, b"third").unwrap();
    assert_eq!(log.len(), 3);
}
