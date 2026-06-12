//! Lifecycle: reopen replay, snapshot acceleration as pure cache, and
//! recovery interaction with the oplog.

use std::fs::{self, OpenOptions};
use std::io::Write;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_refs::{RefChange, RefStore, RefTarget};

fn oid(n: u8) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &[n])
}

fn set(name: &str, old: Option<RefTarget>, new: ObjectId) -> RefChange {
    RefChange {
        name: name.to_owned(),
        old,
        new: Some(RefTarget::Oid(new)),
    }
}

#[test]
fn reopen_replays_to_the_same_state() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = RefStore::open(dir.path()).unwrap();
        store
            .commit("a", 1, &[set("refs/heads/main", None, oid(1))])
            .unwrap();
        store
            .commit(
                "a",
                2,
                &[set("refs/heads/main", Some(RefTarget::Oid(oid(1))), oid(2))],
            )
            .unwrap();
    }
    let store = RefStore::open(dir.path()).unwrap();
    assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(2))));
    assert_eq!(store.len(), 1);
}

#[test]
fn snapshot_is_pure_cache() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = RefStore::open(dir.path()).unwrap();
        store
            .commit("a", 1, &[set("refs/heads/main", None, oid(1))])
            .unwrap();
        store.snapshot().unwrap();
        // state moves past the snapshot point
        store
            .commit(
                "a",
                2,
                &[set("refs/heads/main", Some(RefTarget::Oid(oid(1))), oid(2))],
            )
            .unwrap();
    }

    // stale snapshot: replay must top up past it
    {
        let store = RefStore::open(dir.path()).unwrap();
        assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(2))));
    }

    // corrupt snapshot: ignored, state rebuilt from the log
    let snap = dir.path().join("refs/snapshot");
    let mut bytes = fs::read(&snap).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    fs::write(&snap, &bytes).unwrap();
    {
        let store = RefStore::open(dir.path()).unwrap();
        assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(2))));
    }

    // missing snapshot: same
    fs::remove_file(&snap).unwrap();
    let store = RefStore::open(dir.path()).unwrap();
    assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(2))));
}

#[test]
fn torn_oplog_tail_rolls_back_to_the_previous_transaction() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = RefStore::open(dir.path()).unwrap();
        store
            .commit("a", 1, &[set("refs/heads/main", None, oid(1))])
            .unwrap();
        store
            .commit(
                "a",
                2,
                &[set("refs/heads/main", Some(RefTarget::Oid(oid(1))), oid(2))],
            )
            .unwrap();
    }

    // crash mid-append of a third transaction
    let log = dir.path().join("oplog/log");
    let mut f = OpenOptions::new().append(true).open(&log).unwrap();
    f.write_all(&[0x77; 25]).unwrap();
    drop(f);

    let mut store = RefStore::open(dir.path()).unwrap();
    assert_eq!(
        store.get("refs/heads/main"),
        Some(&RefTarget::Oid(oid(2))),
        "state must be exactly the last durable transaction"
    );
    // and the store keeps working
    store
        .commit(
            "a",
            3,
            &[set("refs/heads/main", Some(RefTarget::Oid(oid(2))), oid(3))],
        )
        .unwrap();
}

#[test]
fn snapshot_from_a_foreign_log_is_orphaned_and_ignored() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    {
        let mut store = RefStore::open(dir_a.path()).unwrap();
        store
            .commit("a", 1, &[set("refs/heads/main", None, oid(1))])
            .unwrap();
        store.snapshot().unwrap();
    }
    {
        let mut store = RefStore::open(dir_b.path()).unwrap();
        store
            .commit("b", 9, &[set("refs/heads/other", None, oid(9))])
            .unwrap();
    }
    // transplant A's snapshot onto B: its op id is not in B's log
    fs::create_dir_all(dir_b.path().join("refs")).unwrap();
    fs::copy(
        dir_a.path().join("refs/snapshot"),
        dir_b.path().join("refs/snapshot"),
    )
    .unwrap();

    let store = RefStore::open(dir_b.path()).unwrap();
    assert!(store.get("refs/heads/main").is_none());
    assert_eq!(store.get("refs/heads/other"), Some(&RefTarget::Oid(oid(9))));
}
