//! The full check reads the whole operation log, including the part an
//! open from a snapshot skips.

use std::fs::OpenOptions;
use std::os::unix::fs::FileExt;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_refs::{RefChange, RefError, RefStore, RefTarget};

fn oid(n: u8) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &[n])
}

/// 300 transactions over 40 branches, a snapshot, then 7 more.
fn filled(dir: &std::path::Path) {
    let mut store = RefStore::open(dir).unwrap();
    for n in 0..307u32 {
        if n == 300 {
            store.snapshot().unwrap();
        }
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
fn a_sound_log_passes_with_every_op_counted() {
    let dir = alt_os::temp::tempdir().unwrap();
    filled(dir.path());
    let store = RefStore::open(dir.path()).unwrap();
    assert!(store.oplog().anchored());
    let checked = store.check().unwrap();
    assert_eq!((checked.ops, checked.refs), (307, 40));
}

#[test]
fn damage_before_the_snapshot_is_found() {
    let dir = alt_os::temp::tempdir().unwrap();
    filled(dir.path());
    let log = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.path().join("oplog/log"))
        .unwrap();
    // a byte a third of the way in: well before the snapshot's op
    let at = log.metadata().unwrap().len() / 3;
    let mut byte = [0u8; 1];
    log.read_exact_at(&mut byte, at).unwrap();
    byte[0] ^= 0x10;
    log.write_all_at(&byte, at).unwrap();

    // an open reads from the snapshot on and does not see it
    let store = RefStore::open(dir.path()).unwrap();
    assert!(store.oplog().anchored());
    assert_eq!(store.len(), 40);
    let err = store.check().unwrap_err();
    assert!(matches!(err, RefError::OpLog(_)), "{err:?}");
}
