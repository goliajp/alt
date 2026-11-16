//! No operation is recorded as synced before the objects it names are.
//!
//! A writer whose fsyncs are deferred (the daemon) has its objects and the
//! ref update naming them in the page cache for a while. Another writer
//! that appends after it and syncs moves the operation log's boundary past
//! that ref update: the objects have to be synced, and recorded as synced,
//! before it does.

use std::path::Path;

use alt_cli::native::Store;
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;
use alt_refs::{RefChange, RefStore, RefTarget};

/// The four append files of the object store, as its marker orders them.
const FILES: [&str; 4] = [
    "store/pack-00000001.altpack",
    "store/manifests/blobmap",
    "tier1",
    "map.alt",
];

fn blob(text: &str) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, text.as_bytes())
}

fn points(name: &str, oid: ObjectId) -> RefChange {
    RefChange {
        name: name.to_owned(),
        old: None,
        new: Some(RefTarget::Oid(oid)),
    }
}

/// The slot of a marker file that holds its state: the one whose checksum
/// holds with the larger serial. Slots of `len` bytes start at `first`.
fn slot(path: &Path, first: usize, len: usize) -> Vec<u8> {
    let data = std::fs::read(path).unwrap();
    let valid = [first, first + len].into_iter().filter_map(|at| {
        let slot = data.get(at..at + len)?;
        let (body, sum) = slot.split_at(len - 8);
        let holds = alt_hash::blake3::hash(body).as_bytes()[..8] == *sum;
        holds.then(|| slot.to_vec())
    });
    valid
        .max_by_key(|slot| u64::from_le_bytes(slot[..8].try_into().unwrap()))
        .expect("a marker with a slot that holds")
}

fn word(slot: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(slot[at..at + 8].try_into().unwrap())
}

/// How far the object store's marker says each of its files is synced.
fn objects_synced(alt_dir: &Path) -> [u64; 4] {
    let slot = slot(&alt_dir.join("odb.durable"), 32, 96);
    [56, 64, 72, 80].map(|at| word(&slot, at))
}

/// How far the operation log's marker says the log is synced.
fn log_synced(alt_dir: &Path) -> u64 {
    word(&slot(&alt_dir.join("oplog/durable"), 16, 36), 20)
}

fn lens(alt_dir: &Path) -> [u64; 4] {
    FILES.map(|name| std::fs::metadata(alt_dir.join(name)).unwrap().len())
}

fn log_len(alt_dir: &Path) -> u64 {
    std::fs::metadata(alt_dir.join("oplog/log")).unwrap().len()
}

/// A repository with one synced commit, then a writer with deferred
/// fsyncs that stores an object and points a ref at it, and syncs
/// neither. Returns that writer, kept open, and where its op ends.
fn deferred_writer(alt_dir: &Path) -> (Store, u64) {
    let first = blob("first\n");
    let mut odb = NativeOdb::open(alt_dir).unwrap();
    odb.put(first, ObjectKind::Blob, b"first\n").unwrap();
    odb.flush().unwrap();
    let mut refs = RefStore::open(alt_dir).unwrap();
    refs.commit("test", 0, &[points("refs/heads/main", first)])
        .unwrap();
    drop((odb, refs));

    let mut deferred = Store::open(alt_dir.to_owned()).unwrap();
    deferred.set_defer_durability(true);
    let synced = objects_synced(alt_dir);
    let second = blob("second\n");
    let odb = deferred.odb_mut();
    odb.put(second, ObjectKind::Blob, b"second\n").unwrap();
    odb.flush().unwrap();
    deferred
        .refs_mut()
        .commit("deferred", 0, &[points("refs/heads/second", second)])
        .unwrap();
    assert_eq!(objects_synced(alt_dir), synced, "the object was synced");
    assert!(log_synced(alt_dir) < log_len(alt_dir), "the op was synced");
    (deferred, log_len(alt_dir))
}

fn assert_objects_recorded(alt_dir: &Path, others_end: u64) {
    assert!(
        log_synced(alt_dir) >= others_end,
        "the other writer's op is not recorded as synced"
    );
    assert_eq!(
        objects_synced(alt_dir),
        lens(alt_dir),
        "the log is recorded as synced past an op whose objects are not"
    );
}

#[test]
fn a_sync_that_passes_another_writers_op_syncs_its_objects_first() {
    let root = alt_os::temp::tempdir().unwrap();
    let alt_dir = root.path().join(".alt");
    let (_deferred, others_end) = deferred_writer(&alt_dir);

    // a ref update and no object of its own
    let mut direct = Store::open(alt_dir.clone()).unwrap();
    let change = points("refs/heads/other", blob("first\n"));
    direct.refs_mut().commit("direct", 0, &[change]).unwrap();
    assert_eq!(log_synced(&alt_dir), log_len(&alt_dir));
    assert_objects_recorded(&alt_dir, others_end);
}

#[test]
fn a_sink_that_passes_another_writers_op_syncs_its_objects_first() {
    let root = alt_os::temp::tempdir().unwrap();
    let alt_dir = root.path().join(".alt");
    let (_deferred, others_end) = deferred_writer(&alt_dir);

    let mut daemon = Store::open(alt_dir.clone()).unwrap();
    daemon.set_defer_durability(true);
    let change = points("refs/heads/other", blob("first\n"));
    daemon.refs_mut().commit("daemon", 0, &[change]).unwrap();
    assert!(log_synced(&alt_dir) < others_end);
    daemon.sink().unwrap().fsync_all().unwrap();
    assert_eq!(log_synced(&alt_dir), log_len(&alt_dir));
    assert_objects_recorded(&alt_dir, others_end);
}

/// A log whose marker is gone gets its boundary from the next append, at
/// the end of the log as it then is.
#[test]
fn a_boundary_set_past_another_writers_op_syncs_its_objects_first() {
    let root = alt_os::temp::tempdir().unwrap();
    let alt_dir = root.path().join(".alt");
    let (_deferred, others_end) = deferred_writer(&alt_dir);
    std::fs::remove_file(alt_dir.join("oplog/durable")).unwrap();

    let mut direct = Store::open(alt_dir.clone()).unwrap();
    let change = points("refs/heads/other", blob("first\n"));
    direct.refs_mut().commit("direct", 0, &[change]).unwrap();
    assert_objects_recorded(&alt_dir, others_end);
}
