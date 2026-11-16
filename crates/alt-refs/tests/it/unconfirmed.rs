//! Ref transactions that were never synced and point refs at objects
//! which are not stored: cut by a writer, the refs following the log.

use std::fs;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_refs::{RefChange, RefError, RefStore, RefTarget, Unconfirmed, named_objects};

fn oid(n: u8) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &[n])
}

fn point(store: &mut RefStore, name: &str, at: u8) {
    let change = RefChange {
        old: store.get(name).cloned(),
        new: Some(RefTarget::Oid(oid(at))),
        name: name.to_owned(),
    };
    store.commit("a", at as u64, &[change]).unwrap();
}

/// `main` at object 1, synced; then, never synced, `main` at object 2,
/// `dev` at object 3, and an op that is no ref transaction.
fn store_with_unsynced(dir: &Path) -> RefStore {
    let mut store = RefStore::open(dir).unwrap();
    point(&mut store, "refs/heads/main", 1);
    store.set_defer_durability(true);
    point(&mut store, "refs/heads/main", 2);
    point(&mut store, "refs/heads/dev", 3);
    store.record_op("a", 9, b"\x40 something else").unwrap();
    store
}

/// Object 2 is not stored.
fn held(op: &alt_oplog::Op) -> Result<bool, RefError> {
    Ok(!named_objects(op)?.contains(&oid(2)))
}

fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut all = Vec::new();
    for sub in ["oplog", "refs"] {
        // no snapshot yet, no directory for it
        for entry in fs::read_dir(dir.join(sub)).into_iter().flatten() {
            let path = entry.unwrap().path();
            all.push((path.display().to_string(), fs::read(&path).unwrap()));
        }
    }
    all.sort();
    all
}

#[test]
fn the_refs_follow_a_log_cut_at_the_first_unsynced_dangling_transaction() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut written = store_with_unsynced(dir.path());
    // a snapshot that tells of the ops about to be cut
    written.snapshot().unwrap();
    drop(written);

    let mut store = RefStore::open(dir.path()).unwrap();
    assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(2))));
    assert!(!store.confirmed());
    let cut = store.settle(held).unwrap().unwrap();
    assert_eq!(cut.ops, 3, "the transaction and what follows it");
    assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(1))));
    assert_eq!(store.get("refs/heads/dev"), None);
    assert_eq!(store.oplog().len(), 1);
    assert_eq!(store.check().unwrap().refs, 1);

    // the store takes transactions on top, and opens to the same refs
    point(&mut store, "refs/heads/main", 4);
    drop(store);
    let store = RefStore::open(dir.path()).unwrap();
    assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(4))));
    assert_eq!(store.get("refs/heads/dev"), None);
    assert!(store.confirmed());
    assert_eq!(store.check().unwrap().ops, 2);
}

#[test]
fn unsynced_transactions_whose_objects_are_stored_stay() {
    let dir = alt_os::temp::tempdir().unwrap();
    drop(store_with_unsynced(dir.path()));
    let before = tree(dir.path());
    let mut store = RefStore::open_read_only(dir.path()).unwrap();
    let all = |_: &alt_oplog::Op| Ok::<_, RefError>(true);
    assert_eq!(store.unconfirmed(all).unwrap(), None);
    assert_eq!(store.settle(all).unwrap(), None);
    assert_eq!(store.get("refs/heads/dev"), Some(&RefTarget::Oid(oid(3))));
    assert_eq!(tree(dir.path()), before);
}

#[test]
fn a_reader_learns_of_them_and_changes_nothing() {
    let dir = alt_os::temp::tempdir().unwrap();
    drop(store_with_unsynced(dir.path()));
    let before = tree(dir.path());
    let store = RefStore::open_read_only(dir.path()).unwrap();
    let found: Option<Unconfirmed> = store.unconfirmed(held).unwrap();
    assert_eq!(found.map(|found| found.ops), Some(3));
    assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(2))));
    assert_eq!(tree(dir.path()), before);
}

#[test]
fn a_store_held_open_follows_a_cut_made_by_another() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut held_open = store_with_unsynced(dir.path());
    held_open.set_defer_durability(false);
    let mut other = RefStore::open(dir.path()).unwrap();
    assert!(other.settle(held).unwrap().is_some());
    point(&mut other, "refs/heads/other", 5);

    held_open.refresh().unwrap();
    assert_eq!(
        held_open.get("refs/heads/main"),
        Some(&RefTarget::Oid(oid(1)))
    );
    assert_eq!(held_open.get("refs/heads/dev"), None);
    assert_eq!(
        held_open.get("refs/heads/other"),
        Some(&RefTarget::Oid(oid(5)))
    );
    point(&mut held_open, "refs/heads/main", 6);
    let store = RefStore::open(dir.path()).unwrap();
    assert_eq!(store.check().unwrap().ops, 3);
}
