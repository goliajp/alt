//! A transaction that fails once it is in the log is told apart from one
//! that never got there: what was stored for the first has to stay.

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_refs::{RefChange, RefError, RefStore, RefTarget};

const MAIN: &str = "refs/heads/main";

fn oid(n: u8) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &[n])
}

fn points(old: Option<u8>, new: u8) -> RefChange {
    RefChange {
        name: MAIN.to_owned(),
        old: old.map(|n| RefTarget::Oid(oid(n))),
        new: Some(RefTarget::Oid(oid(new))),
    }
}

#[test]
fn a_failure_after_the_append_names_the_op_it_left_in_the_log() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut store = RefStore::open(dir.path()).unwrap();
    store.commit("a", 1, &[points(None, 1)]).unwrap();

    // refused before the append: the log is as it was
    let err = store.commit("a", 2, &[points(Some(9), 2)]).unwrap_err();
    assert!(matches!(err, RefError::Conflict { .. }), "{err:?}");
    assert_eq!(RefStore::open(dir.path()).unwrap().oplog().len(), 1);

    // no sync gets its lock: a directory is not opened for writing
    let lock = dir.path().join("oplog/sync.lock");
    let _ = std::fs::remove_file(&lock);
    std::fs::create_dir(&lock).unwrap();
    let err = store.commit("a", 3, &[points(Some(1), 2)]).unwrap_err();
    let RefError::Recorded { op, cause } = err else {
        panic!("{err:?}");
    };
    assert!(matches!(*cause, RefError::OpLog(_)), "{cause:?}");
    std::fs::remove_dir(&lock).unwrap();

    // as another process reads it
    let other = RefStore::open(dir.path()).unwrap();
    assert_eq!(other.head_op(), Some(op));
    assert_eq!(other.get(MAIN), Some(&RefTarget::Oid(oid(2))));
}

/// An error that is not [`RefError::Recorded`] says the transaction is
/// not in the log, and no more: the append took in what another writer
/// left before it was refused, here the bytes of a record that never
/// was whole, which are cut off.
#[test]
fn a_transaction_refused_before_the_append_may_leave_the_log_shorter() {
    use std::io::Write;
    let dir = alt_os::temp::tempdir().unwrap();
    let mut store = RefStore::open(dir.path()).unwrap();
    store.commit("a", 1, &[points(None, 1)]).unwrap();
    let log = dir.path().join("oplog/log");
    let whole = std::fs::metadata(&log).unwrap().len();
    let mut torn = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    torn.write_all(&[0x40, 0, 0, 0, 1, 2, 3]).unwrap();
    drop(torn);

    let err = store.commit("a", 2, &[points(Some(9), 2)]).unwrap_err();
    assert!(matches!(err, RefError::Conflict { .. }), "{err:?}");
    assert_eq!(std::fs::metadata(&log).unwrap().len(), whole);
    assert_eq!(RefStore::open(dir.path()).unwrap().oplog().len(), 1);
}

/// An op that is no ref transaction is not told apart that way: the
/// error of its sync comes with the op in the log.
#[test]
fn an_op_whose_sync_fails_is_in_the_log_and_its_error_does_not_say_so() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut store = RefStore::open(dir.path()).unwrap();
    store.commit("a", 1, &[points(None, 1)]).unwrap();
    let lock = dir.path().join("oplog/sync.lock");
    let _ = std::fs::remove_file(&lock);
    std::fs::create_dir(&lock).unwrap();
    let err = store
        .record_op("a", 2, b"\x7fan op of another kind")
        .unwrap_err();
    assert!(matches!(err, RefError::OpLog(_)), "{err:?}");
    std::fs::remove_dir(&lock).unwrap();
    assert_eq!(RefStore::open(dir.path()).unwrap().oplog().len(), 2);
}
