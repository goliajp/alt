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
