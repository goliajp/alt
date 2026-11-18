//! The snapshot's note of how far the log is synced: a writer raises it
//! in place after it syncs, an open whose log is no longer than it says
//! does not read the log's marker, and a snapshot without one gets one.

use std::fs;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_oplog::parse_note;
use alt_refs::{RefChange, RefStore, RefTarget};

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

fn snapshot(dir: &Path) -> Vec<u8> {
    fs::read(dir.join("refs/snapshot")).unwrap()
}

/// The version of the snapshot and what it notes.
fn note(dir: &Path) -> (u8, Option<u64>) {
    let data = snapshot(dir);
    (data[4], parse_note(&data[5..21]))
}

fn log_len(dir: &Path) -> u64 {
    fs::metadata(dir.join("oplog/log")).unwrap().len()
}

/// Three synced transactions and a snapshot of them.
fn synced(dir: &Path) -> RefStore {
    let mut store = RefStore::open(dir).unwrap();
    for n in 1..=3 {
        point(&mut store, "refs/heads/main", n);
    }
    store.snapshot().unwrap();
    store
}

#[test]
fn a_writer_notes_in_the_snapshot_how_far_it_synced_the_log() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut store = synced(dir.path());
    assert_eq!(note(dir.path()), (3, Some(log_len(dir.path()))));
    let before = snapshot(dir.path());
    point(&mut store, "refs/heads/main", 4);
    assert_eq!(note(dir.path()), (3, Some(log_len(dir.path()))));
    // in place: the rest of the snapshot is the one of three transactions
    let after = snapshot(dir.path());
    assert_eq!((&after[..5], &after[21..]), (&before[..5], &before[21..]));
}

#[test]
fn a_log_no_longer_than_the_note_says_is_not_looked_up_in_the_marker() {
    let dir = alt_os::temp::tempdir().unwrap();
    drop(synced(dir.path()));
    // without the marker only the note says the log is synced
    fs::remove_file(dir.path().join("oplog/durable")).unwrap();
    let store = RefStore::open_reading(dir.path()).unwrap();
    assert!(store.confirmed());
    assert!(!store.unsettled());
    assert_eq!(store.unheld(None, |_| false).unwrap(), None);
    drop(store);
    // a note that does not read is no note
    let mut data = snapshot(dir.path());
    data[7] ^= 1;
    fs::write(dir.path().join("refs/snapshot"), data).unwrap();
    let store = RefStore::open_reading(dir.path()).unwrap();
    assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(3))));
    assert!(!store.confirmed());
}

#[test]
fn a_snapshot_without_a_note_is_read_and_gets_one_from_the_next_reader() {
    let dir = alt_os::temp::tempdir().unwrap();
    drop(synced(dir.path()));
    // the same snapshot as the version before the note wrote it
    let noted = snapshot(dir.path());
    let mut older = [&noted[..4], &[2], &noted[21..noted.len() - 8]].concat();
    // the block table holds offsets into the file
    let blocks = u32::from_le_bytes(older[65..69].try_into().unwrap()) as usize;
    for entry in older[69..69 + 4 * blocks].as_chunks_mut::<4>().0 {
        *entry = (u32::from_le_bytes(*entry) - 16).to_le_bytes();
    }
    let sum = alt_hash::blake3::hash(&older);
    older.extend_from_slice(&sum.as_bytes()[..8]);
    fs::write(dir.path().join("refs/snapshot"), &older).unwrap();

    let check = RefStore::open_read_only(dir.path()).unwrap();
    assert_eq!(check.get("refs/heads/main"), Some(&RefTarget::Oid(oid(3))));
    assert!(check.confirmed(), "by the marker");
    drop(check);
    assert_eq!(snapshot(dir.path()), older, "a check writes nothing");

    let store = RefStore::open_reading(dir.path()).unwrap();
    assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(3))));
    assert_eq!(note(dir.path()), (3, Some(log_len(dir.path()))));
    let rewritten = snapshot(dir.path());
    drop(store);
    drop(RefStore::open_reading(dir.path()).unwrap());
    assert_eq!(snapshot(dir.path()), rewritten, "once");
}

#[test]
fn a_note_that_fell_behind_a_synced_log_is_brought_up_by_the_next_reader() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut store = synced(dir.path());
    let behind = snapshot(dir.path());
    point(&mut store, "refs/heads/main", 4);
    drop(store);
    // the write of the note was lost
    fs::write(dir.path().join("refs/snapshot"), behind).unwrap();
    assert!(note(dir.path()).1 < Some(log_len(dir.path())));
    let store = RefStore::open_reading(dir.path()).unwrap();
    assert!(store.confirmed());
    assert_eq!(note(dir.path()), (3, Some(log_len(dir.path()))));
}

/// `main` at object 1, synced and in a snapshot; then, never synced,
/// `main` at object 2 and `dev` at object 3. Object 2 is not stored.
/// (Opening the store without those operations ends the reads of the log
/// for the whole process, by the file's inode: that is left to the tests
/// that run the command, each in a process of its own.)
#[test]
fn a_reader_is_told_the_unsynced_operations_that_name_what_is_not_stored() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut store = RefStore::open(dir.path()).unwrap();
    point(&mut store, "refs/heads/main", 1);
    store.snapshot().unwrap();
    store.set_defer_durability(true);
    point(&mut store, "refs/heads/main", 2);
    point(&mut store, "refs/heads/dev", 3);
    drop(store);
    let log = fs::read(dir.path().join("oplog/log")).unwrap();
    let marker = fs::read(dir.path().join("oplog/durable")).unwrap();

    let stored = |named: &ObjectId| *named != oid(2);
    let store = RefStore::open_reading(dir.path()).unwrap();
    assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(2))));
    assert!(!store.confirmed());
    assert_eq!(
        store.unheld(None, stored).unwrap().map(|found| found.ops),
        Some(2)
    );
    assert_eq!(store.unheld(None, |_| true).unwrap(), None);
    // nothing of the log changed
    assert_eq!(fs::read(dir.path().join("oplog/log")).unwrap(), log);
    assert_eq!(fs::read(dir.path().join("oplog/durable")).unwrap(), marker);
}
