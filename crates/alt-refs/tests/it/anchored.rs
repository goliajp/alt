//! The snapshot is anchored where the log's marker records it as synced
//! and no further: a record past that boundary which does not read ends
//! the log for every open, whatever a snapshot was written after.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_refs::{RefChange, RefError, RefStore, RefTarget};

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

fn log_len(dir: &Path) -> u64 {
    fs::metadata(dir.join("oplog/log")).unwrap().len()
}

/// Where in the log the op ends that the snapshot is anchored at.
fn anchored_at(dir: &Path) -> u64 {
    let snapshot = fs::read(dir.join("refs/snapshot")).unwrap();
    assert_eq!((&snapshot[..4], snapshot[4]), (&b"ALTS"[..], 3));
    u64::from_le_bytes(snapshot[61..69].try_into().unwrap())
}

fn target(store: &RefStore, name: &str) -> Option<ObjectId> {
    store.resolve(name).unwrap()
}

/// `main` at object 1 and `dev` at object 2, synced; then `main` at 3,
/// `dev` deleted and `topic` at 4, none of them synced, with ops that
/// are no ref transactions among them. Where the synced part ends, and
/// where each unsynced op does.
fn store_with_unsynced(dir: &Path) -> (RefStore, u64, Vec<u64>) {
    let mut store = RefStore::open(dir).unwrap();
    point(&mut store, "refs/heads/main", 1);
    point(&mut store, "refs/heads/dev", 2);
    let synced = log_len(dir);
    store.set_defer_durability(true);
    let mut ends = Vec::new();
    point(&mut store, "refs/heads/main", 3);
    ends.push(log_len(dir));
    store.record_op("a", 9, b"\x40 something else").unwrap();
    ends.push(log_len(dir));
    let gone = RefChange {
        name: "refs/heads/dev".to_owned(),
        old: store.get("refs/heads/dev").cloned(),
        new: None,
    };
    store.commit("a", 10, &[gone]).unwrap();
    ends.push(log_len(dir));
    point(&mut store, "refs/heads/topic", 4);
    ends.push(log_len(dir));
    (store, synced, ends)
}

fn at_the_head(store: &RefStore) {
    assert_eq!(target(store, "refs/heads/main"), Some(oid(3)));
    assert_eq!(target(store, "refs/heads/dev"), None);
    assert_eq!(target(store, "refs/heads/topic"), Some(oid(4)));
    store.check().unwrap();
}

#[test]
fn a_snapshot_written_over_unsynced_ops_is_anchored_before_them_and_the_refs_stay() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (mut store, synced, ends) = store_with_unsynced(dir.path());
    store.snapshot().unwrap();
    assert_eq!(anchored_at(dir.path()), synced);
    // the store goes on from the refs it had
    at_the_head(&store);
    point(&mut store, "refs/heads/topic", 5);
    assert_eq!(target(&store, "refs/heads/topic"), Some(oid(5)));
    std::mem::forget(store);

    // an open begins at the snapshot and folds in what follows it
    let opened = RefStore::open_reading(dir.path()).unwrap();
    assert_eq!(target(&opened, "refs/heads/main"), Some(oid(3)));
    assert_eq!(target(&opened, "refs/heads/dev"), None);
    assert_eq!(target(&opened, "refs/heads/topic"), Some(oid(5)));
    opened.check().unwrap();
    assert_eq!(anchored_at(dir.path()), synced);

    // once the ops are synced the snapshot is anchored at the head
    let mut synced_all = RefStore::open(dir.path()).unwrap();
    point(&mut synced_all, "refs/heads/topic", 6);
    synced_all.snapshot().unwrap();
    assert_eq!(anchored_at(dir.path()), log_len(dir.path()));
    assert!(log_len(dir.path()) > *ends.last().unwrap());
}

/// A snapshot anchored past the boundary, as a build that anchored it at
/// the head of the log wrote it: made here of one written when all was
/// synced, the marker and the note then put back to where they were
/// before the last ops.
fn anchored_past_the_boundary(dir: &Path) -> (u64, Vec<u64>) {
    let (store, synced, ends) = store_with_unsynced(dir);
    let marker = fs::read(dir.join("oplog/durable")).unwrap();
    store.sync_handle().unwrap().fsync().unwrap();
    let mut store = store;
    store.snapshot().unwrap();
    at_the_head(&store);
    std::mem::forget(store);
    assert_eq!(anchored_at(dir), *ends.last().unwrap());
    fs::write(dir.join("oplog/durable"), marker).unwrap();
    let snapshot = fs::OpenOptions::new()
        .write(true)
        .open(dir.join("refs/snapshot"));
    let note = alt_oplog::encode_note(synced);
    snapshot.unwrap().write_all_at(&note, 5).unwrap();
    (synced, ends)
}

#[test]
fn a_record_that_does_not_read_before_a_snapshot_anchored_past_the_boundary_ends_the_log() {
    for lost in 0..3 {
        let dir = alt_os::temp::tempdir().unwrap();
        let (synced, ends) = anchored_past_the_boundary(dir.path());
        // the snapshot is not begun at, whole as the log is
        at_the_head(&RefStore::open_read_only(dir.path()).unwrap());
        let from = [synced, ends[0], ends[1]][lost];
        let log = fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join("oplog/log"));
        log.unwrap().write_all_at(&[0xff; 24], from + 8).unwrap();
        let before = fs::read(dir.path().join("oplog/log")).unwrap();

        let stored = |_: &ObjectId| true;
        let read = RefStore::open_confirmed(dir.path(), None, stored).unwrap();
        let main = if lost == 0 { 1 } else { 3 };
        assert_eq!(target(&read, "refs/heads/main"), Some(oid(main)), "{lost}");
        assert_eq!(target(&read, "refs/heads/dev"), Some(oid(2)), "{lost}");
        assert_eq!(target(&read, "refs/heads/topic"), None, "{lost}");
        assert_eq!(read.unread().unwrap(), 3 - lost, "{lost}");
        assert!(fs::read(dir.path().join("oplog/log")).unwrap() == before);
        drop(read);

        let mut written = RefStore::open(dir.path()).unwrap();
        let cut = written.settle(|_| Ok::<_, RefError>(true)).unwrap();
        assert_eq!(cut, None, "{lost}");
        point(&mut written, "refs/heads/topic", 7);
        assert_eq!(
            target(&written, "refs/heads/main"),
            Some(oid(main)),
            "{lost}"
        );
        assert_eq!(target(&written, "refs/heads/dev"), Some(oid(2)), "{lost}");
        written.check().unwrap();
        let discarded: usize = written
            .take_discarded()
            .iter()
            .map(|cut| cut.ops.len())
            .sum();
        assert_eq!(discarded, 3 - lost, "{lost}");
    }
}
