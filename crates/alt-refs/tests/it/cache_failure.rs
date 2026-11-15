//! The snapshot is a cache: a place where it cannot be written (a read-only
//! file system, a full disk) costs the next open its shortcut, never the
//! command its result.

use std::os::unix::fs::symlink;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_refs::{RefChange, RefStore, RefTarget};

fn oid(n: u8) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &[n])
}

fn churn(store: &mut RefStore, from: u32, count: u32) {
    for n in from..from + count {
        let name = format!("refs/heads/b{}", n % 40);
        let change = RefChange {
            old: store.get(&name).cloned(),
            new: Some(RefTarget::Oid(oid((n % 251) as u8))),
            name,
        };
        store.commit("a", n as u64, &[change]).unwrap();
    }
}

#[test]
fn a_snapshot_that_cannot_be_written_fails_neither_open_nor_commit() {
    let dir = alt_os::temp::tempdir().unwrap();
    {
        let mut store = RefStore::open(dir.path()).unwrap();
        churn(&mut store, 0, 400);
    }
    // nothing can be created under a dangling link, whoever tries
    let refs = dir.path().join("refs");
    std::fs::remove_dir_all(&refs).unwrap();
    symlink(dir.path().join("nowhere/at/all"), &refs).unwrap();

    // the open replays the whole log and would now write a snapshot
    let mut store = RefStore::open(dir.path()).unwrap();
    assert_eq!(store.len(), 40);
    // enough new operations for another snapshot to fall due
    churn(&mut store, 400, 1100);
    assert_eq!(store.oplog().len(), 1500);
    drop(store);
    assert_eq!(RefStore::open(dir.path()).unwrap().oplog().len(), 1500);
}
