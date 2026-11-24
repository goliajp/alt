//! The ref store over a version 2 log: what the tests of the snapshot's
//! anchor, of unconfirmed transactions and of the full check hold it to
//! over a version 1 log, where the log says itself how far it is synced.

use std::fs;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_oplog::v2::block::FileHeader;
use alt_oplog::v2::frame::{Seed, encode, parse};
use alt_refs::{RefChange, RefError, RefStore, RefTarget, named_objects};

pub fn oid(n: u8) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &[n])
}

pub fn point(store: &mut RefStore, name: &str, at: u8) {
    let change = RefChange {
        old: store.get(name).cloned(),
        new: Some(RefTarget::Oid(oid(at))),
        name: name.to_owned(),
    };
    store.commit("a", at as u64, &[change]).unwrap();
}

/// A store over a new version 2 log with blocks of 4 KiB.
pub fn new_store(dir: &Path) -> RefStore {
    alt_oplog::v2::create_with(&dir.join("oplog"), 12).unwrap();
    let store = RefStore::open(dir).unwrap();
    assert_eq!(store.oplog().format(), 2);
    store
}

pub fn boot(n: u8) {
    alt_oplog::v2::in_boot(Some([n; 16]));
}

fn snapshot(dir: &Path) -> Option<Vec<u8>> {
    fs::read(dir.join("refs/snapshot")).ok()
}

fn refs(store: &RefStore) -> Vec<(String, RefTarget)> {
    let owned = |(name, target): (&str, &RefTarget)| (name.to_owned(), target.clone());
    store.iter().map(owned).collect()
}

/// 300 transactions over 40 branches and 7 more: enough of a log for the
/// store to have written a snapshot by itself.
fn filled(dir: &Path) -> Vec<(String, RefTarget)> {
    let mut store = new_store(dir);
    for n in 0..307u32 {
        point(
            &mut store,
            &format!("refs/heads/b{}", n % 40),
            (n % 251) as u8,
        );
    }
    refs(&store)
}

#[test]
fn transactions_are_frames_that_name_the_one_before_them() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut store = new_store(dir.path());
    point(&mut store, "refs/heads/main", 1);
    store.record_op("a", 5, b"\x40 no ref transaction").unwrap();
    point(&mut store, "refs/heads/main", 2);
    point(&mut store, "refs/heads/dev", 3);
    let state = store.oplog().state().unwrap().clone();
    assert_eq!((state.seq, state.linked), (4, 3));
    assert_eq!(state.linked_at, store.oplog().anchor().unwrap().start);
    let written = refs(&store);
    drop(store);
    let store = RefStore::open(dir.path()).unwrap();
    assert_eq!((refs(&store), store.len()), (written, 2));
    assert!(store.confirmed() && !store.unsettled());
    let checked = store.check().unwrap();
    assert_eq!((checked.ops, checked.refs), (4, 2));
    assert_eq!(snapshot(dir.path()), None, "too short a log for one");
}

#[test]
fn a_snapshot_lets_an_open_read_the_end_of_the_log_alone_and_the_full_check_reads_all() {
    let dir = alt_os::temp::tempdir().unwrap();
    let written = filled(dir.path());
    assert_eq!(snapshot(dir.path()).unwrap()[..5], *b"ALTS\x04");
    let len = fs::metadata(dir.path().join("oplog/log")).unwrap().len();
    assert!(len > 15 * 4096);
    let store = RefStore::open(dir.path()).unwrap();
    assert!(store.oplog().anchored());
    assert_eq!(refs(&store), written);
    // the last block and the ref transactions since the snapshot
    assert!(store.oplog().bytes_read() < 64 + 4096 + 16_384 + 4096);
    let checked = store.check().unwrap();
    assert_eq!((checked.ops, checked.refs), (307, 40));

    // an op whose frame reads but whose id is not the hash of its body:
    // only the full check hashes
    let log = dir.path().join("oplog/log");
    let sound = fs::read(&log).unwrap();
    let head = store.oplog().anchor().unwrap();
    let layout = FileHeader::decode(&sound).unwrap().layout();
    let seed = Seed::new(&layout.id, 0);
    let frame = parse(&seed, &sound[head.start as usize..head.end as usize]).unwrap();
    let mut renamed = frame.op.unwrap();
    renamed.0[0] ^= 1;
    let frame = encode(&seed, &frame.head, &frame.ext, frame.body, Some(&renamed)).unwrap();
    let mut bytes = sound.clone();
    bytes[head.start as usize..head.end as usize].copy_from_slice(&frame);
    fs::write(&log, bytes).unwrap();
    let store = RefStore::open(dir.path()).unwrap();
    assert_eq!(refs(&store), written);
    assert!(matches!(store.check(), Err(RefError::OpLog(_))));
    fs::write(&log, sound).unwrap();

    // damage before the snapshot's op: an open does not read it, the full
    // check does
    let mut bytes = fs::read(&log).unwrap();
    bytes[len as usize / 3] ^= 0x10;
    fs::write(&log, bytes).unwrap();
    let store = RefStore::open(dir.path()).unwrap();
    assert!(store.oplog().anchored());
    assert_eq!(refs(&store), written);
    let err = store.check().unwrap_err();
    assert!(matches!(err, RefError::OpLog(_)), "{err:?}");
}

#[test]
fn without_a_snapshot_that_reads_the_log_is_read_from_its_first_op() {
    let dir = alt_os::temp::tempdir().unwrap();
    let written = filled(dir.path());
    let path = dir.path().join("refs/snapshot");
    let whole = fs::read(&path).unwrap();
    let mut other = whole.clone();
    other[40] ^= 1;
    for left in [None, Some(other), Some(whole[..whole.len() - 3].to_vec())] {
        match left {
            Some(bytes) => fs::write(&path, bytes).unwrap(),
            None => fs::remove_file(&path).unwrap(),
        }
        // an open that writes no snapshot, then one that does
        let store = RefStore::open_read_only(dir.path()).unwrap();
        assert!(!store.oplog().anchored());
        assert_eq!(refs(&store), written);
        assert_eq!(store.check().unwrap().ops, 307);
        drop(store);
        let store = RefStore::open(dir.path()).unwrap();
        assert!(!store.oplog().anchored());
        assert_eq!(refs(&store), written);
        drop(store);
        assert_eq!(snapshot(dir.path()).unwrap().len(), whole.len());
        assert!(RefStore::open(dir.path()).unwrap().oplog().anchored());
    }
}

/// In boot 1: `main` at object 1, synced; then, never synced, `main` at
/// object 2, `dev` at object 3, and an op that is no ref transaction.
pub fn store_with_unsynced(dir: &Path) -> RefStore {
    boot(1);
    let mut store = new_store(dir);
    point(&mut store, "refs/heads/main", 1);
    store.set_defer_durability(true);
    point(&mut store, "refs/heads/main", 2);
    point(&mut store, "refs/heads/dev", 3);
    store.record_op("a", 9, b"\x40 something else").unwrap();
    store
}

/// Object 2 is not stored.
pub fn held(op: &alt_oplog::Op) -> Result<bool, RefError> {
    Ok(!named_objects(op)?.contains(&oid(2)))
}

#[test]
fn a_snapshot_anchored_past_what_is_synced_is_begun_at_only_in_the_boot_that_wrote_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut store = store_with_unsynced(dir.path());
    let synced = store.oplog().bounded_to().unwrap();
    store.snapshot().unwrap();
    let head = store.oplog().anchor().unwrap();
    assert!(head.end > synced, "the snapshot's op was never synced");
    let written = refs(&store);
    drop(store);
    // the boot that wrote it begins at it: nothing is left to replay
    let store = RefStore::open(dir.path()).unwrap();
    assert!(store.oplog().anchored() && store.oplog().recent().is_empty());
    assert_eq!(refs(&store), written);
    drop(store);

    // after the machine went down it is not taken on trust
    boot(2);
    let store = RefStore::open_read_only(dir.path()).unwrap();
    assert!(!store.oplog().anchored());
    assert_eq!(store.oplog().recent().len(), 4);
    assert_eq!(refs(&store), written, "the log still holds every op");
    assert!(store.unsettled());
    // and where one of those ops is gone, the refs are those before it
    let log = dir.path().join("oplog/log");
    let mut bytes = fs::read(&log).unwrap();
    bytes[head.start as usize - 20] ^= 1;
    fs::write(&log, bytes).unwrap();
    for n in [1, 2] {
        boot(n);
        let store = RefStore::open_read_only(dir.path()).unwrap();
        assert!(!store.oplog().anchored(), "boot {n}");
        assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(2))));
        assert_eq!(store.get("refs/heads/dev"), None, "boot {n}");
        assert_eq!(store.unread().unwrap(), 1);
    }

    // one anchored within what is synced is begun at in any boot
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut store = new_store(dir.path());
    point(&mut store, "refs/heads/main", 1);
    store.snapshot().unwrap();
    drop(store);
    boot(2);
    assert!(RefStore::open(dir.path()).unwrap().oplog().anchored());
}

#[test]
fn the_refs_follow_a_log_cut_at_the_first_dangling_transaction_another_boot_left() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut written = store_with_unsynced(dir.path());
    // a snapshot that tells of the ops about to be cut
    written.snapshot().unwrap();
    // the boot that wrote them has nothing to look at
    assert!(!written.unsettled());
    assert_eq!(written.settle(held).unwrap(), None);
    drop(written);

    boot(2);
    // a reader learns of them and changes nothing in the log
    let before = fs::read(dir.path().join("oplog/log")).unwrap();
    let reader = RefStore::open_read_only(dir.path()).unwrap();
    assert!(reader.unsettled() && !reader.confirmed());
    let unheld = reader.unheld(None, |object| *object != oid(2));
    assert_eq!(unheld.unwrap().unwrap().ops, 3);
    assert_eq!(fs::read(dir.path().join("oplog/log")).unwrap(), before);
    drop(reader);

    let mut store = RefStore::open(dir.path()).unwrap();
    assert!(store.unsettled());
    assert_eq!(store.settle(held).unwrap().unwrap().ops, 3);
    assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(1))));
    assert_eq!(store.get("refs/heads/dev"), None);
    let cut = store.take_discarded();
    assert_eq!((cut.len(), cut[0].ops.len(), cut[0].unread), (1, 3, 0));
    assert!(!store.unsettled());
    point(&mut store, "refs/heads/main", 4);
    drop(store);
    let store = RefStore::open(dir.path()).unwrap();
    assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(4))));
    assert_eq!(store.len(), 1);
    assert!(store.confirmed());
    store.check().unwrap();
}

#[test]
fn unsynced_transactions_whose_objects_are_stored_stay() {
    let dir = alt_os::temp::tempdir().unwrap();
    drop(store_with_unsynced(dir.path()));
    boot(2);
    let mut store = RefStore::open(dir.path()).unwrap();
    let mut asked = 0;
    let cut = store.settle(|_| {
        asked += 1;
        Ok::<_, RefError>(true)
    });
    // the op that is no ref transaction was written by a writer that syncs
    assert_eq!((cut.unwrap(), asked), (None, 3));
    assert!(!store.unsettled() && !store.confirmed());
    assert_eq!(store.get("refs/heads/dev"), Some(&RefTarget::Oid(oid(3))));
    assert!(!RefStore::open(dir.path()).unwrap().unsettled());
}
