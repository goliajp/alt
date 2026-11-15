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
    let dir = alt_os::temp::tempdir().unwrap();
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
    let dir = alt_os::temp::tempdir().unwrap();
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
    let dir = alt_os::temp::tempdir().unwrap();
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
    let dir_a = alt_os::temp::tempdir().unwrap();
    let dir_b = alt_os::temp::tempdir().unwrap();
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

fn all_refs(store: &RefStore) -> Vec<(String, RefTarget)> {
    store
        .iter()
        .map(|(name, target)| (name.to_owned(), target.clone()))
        .collect()
}

/// Commits `count` transactions that each move one of 40 branches.
fn churn(store: &mut RefStore, from: u32, count: u32) {
    for n in from..from + count {
        let name = format!("refs/heads/b{}", n % 40);
        let old = store.get(&name).cloned();
        store
            .commit("a", n as u64, &[set(&name, old, oid((n % 251) as u8))])
            .unwrap();
    }
}

#[test]
fn open_from_a_snapshot_matches_a_full_replay() {
    let dir = alt_os::temp::tempdir().unwrap();
    let snapshot = dir.path().join("refs/snapshot");
    {
        let mut store = RefStore::open(dir.path()).unwrap();
        churn(&mut store, 0, 300);
        store.snapshot().unwrap();
        churn(&mut store, 300, 7);
    }
    let anchored = RefStore::open(dir.path()).unwrap();
    assert!(anchored.oplog().anchored());
    assert_eq!(anchored.oplog().recent().len(), 7);
    assert_eq!(anchored.oplog().len(), 307);
    let with_snapshot = all_refs(&anchored);
    let head = anchored.head_op();
    drop(anchored);

    fs::remove_file(&snapshot).unwrap();
    let replayed = RefStore::open(dir.path()).unwrap();
    assert!(!replayed.oplog().anchored());
    assert_eq!(all_refs(&replayed), with_snapshot);
    assert_eq!(replayed.head_op(), head);
}

#[test]
fn a_reader_refreshes_a_snapshot_that_fell_behind() {
    let dir = alt_os::temp::tempdir().unwrap();
    let snapshot = dir.path().join("refs/snapshot");
    {
        let mut store = RefStore::open(dir.path()).unwrap();
        churn(&mut store, 0, 5);
        assert!(!snapshot.exists(), "a short log needs no snapshot");
        // enough log bytes for a snapshot to pay off
        churn(&mut store, 5, 400);
    }
    assert!(snapshot.exists());
    let expect = all_refs(&RefStore::open(dir.path()).unwrap());

    // losing it costs one full replay; that open writes it again
    fs::remove_file(&snapshot).unwrap();
    assert!(!RefStore::open(dir.path()).unwrap().oplog().anchored());
    assert!(snapshot.exists());
    let store = RefStore::open(dir.path()).unwrap();
    assert!(store.oplog().anchored());
    assert!(store.oplog().recent().is_empty());
    assert_eq!(all_refs(&store), expect);
}

#[test]
fn a_snapshot_without_a_log_position_is_still_read() {
    // the layout written before snapshots recorded where their op sits
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("refs/snapshot");
    let (at_op, expect) = {
        let mut store = RefStore::open(dir.path()).unwrap();
        churn(&mut store, 0, 400);
        let state = (store.head_op().unwrap(), all_refs(&store));
        churn(&mut store, 400, 3);
        state
    };
    let mut old = b"ALTS\x01".to_vec();
    old.extend_from_slice(&at_op.0);
    old.extend_from_slice(&(expect.len() as u32).to_le_bytes());
    for (name, target) in &expect {
        let RefTarget::Oid(target) = target else {
            panic!("only direct refs here")
        };
        old.extend_from_slice(&(name.len() as u16).to_le_bytes());
        old.extend_from_slice(name.as_bytes());
        old.extend_from_slice(&[1, 1]);
        old.extend_from_slice(target.as_bytes());
    }
    let check = alt_hash::blake3::hash(&old);
    old.extend_from_slice(&check.as_bytes()[..8]);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, &old).unwrap();

    let store = RefStore::open(dir.path()).unwrap();
    assert!(!store.oplog().anchored(), "the whole log is replayed once");
    assert_eq!(store.oplog().len(), 403);
    let current = all_refs(&store);
    assert!(current != expect, "three transactions follow the snapshot");
    drop(store);
    assert!(fs::read(&path).unwrap() != old, "the open rewrote it");
    let store = RefStore::open(dir.path()).unwrap();
    assert!(store.oplog().anchored());
    assert_eq!(all_refs(&store), current);
}

#[test]
fn writers_through_separate_handles_see_each_others_transactions() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut a = RefStore::open(dir.path()).unwrap();
    churn(&mut a, 0, 400);
    // b opens at the snapshot a wrote on the way
    let mut b = RefStore::open(dir.path()).unwrap();
    assert!(b.oplog().anchored());
    churn(&mut a, 400, 30);
    b.refresh().unwrap();
    assert_eq!(all_refs(&a), all_refs(&b));
    churn(&mut b, 430, 30);
    a.refresh().unwrap();
    assert_eq!(all_refs(&a), all_refs(&b));
    assert_eq!(a.oplog().len(), 460);
    assert_eq!(b.oplog().len(), 460);
    assert_eq!(b.oplog().ops().unwrap().len(), 460);
    assert_eq!(all_refs(&RefStore::open(dir.path()).unwrap()), all_refs(&a));
}
