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

/// A candidate record is a file of its own, written with its own fsync,
/// and names the tree stored for it. A store whose fsyncs are left to a
/// sink, as the daemon holds it, syncs that tree before the record.
#[test]
fn a_candidate_record_is_written_after_its_tree_is_synced() {
    use alt_cli::cli::CandidateOp;
    use alt_cli::native::{Identity, NativeRepo, resolve_workspace};

    let f = super::task_delivery::fixture("exit 0\n");
    std::fs::write(f.workspace.join("source.txt"), "another candidate\n").unwrap();
    super::task_delivery::ok(&f.workspace, &["add", "source.txt"]);

    let (alt_dir, coord) = resolve_workspace(&f.workspace, None).unwrap();
    let mut store = Store::open(alt_dir).unwrap();
    store.set_defer_durability(true);
    let user = ("USER".to_owned(), "runner-test".to_owned());
    let id = Identity::from_map(&[user]);
    let mut repo = NativeRepo::attach(&mut store, coord, id, Some([7; 16]));
    let create = CandidateOp::Create {
        task: f.task.clone(),
        parent: None,
    };
    let mut out = Vec::new();
    repo.candidate_command(&create, true, &mut out).unwrap();
    assert!(
        store.odb().confirmed().unwrap(),
        "the record was written before the tree it names was synced"
    );
}

/// A merge that stops on a conflict leaves an index, written with its own
/// fsync, that names the blobs of the paths it did merge.
#[test]
fn the_index_of_a_stopped_merge_is_written_after_its_blobs_are_synced() {
    use super::unconfirmed::{alt, ok};
    use alt_cli::native::{Identity, NativeRepo, resolve_workspace};

    let root = alt_os::temp::tempdir().unwrap();
    let repo = root.path();
    let lines = |first: &str, last: &str| format!("{first}\n2\n3\n4\n5\n6\n7\n8\n{last}\n");
    let commit = |both: &str, first: &str, last: &str| {
        std::fs::write(repo.join("both.txt"), both).unwrap();
        std::fs::write(repo.join("each.txt"), lines(first, last)).unwrap();
        ok(alt(repo, &["add", "."]));
        ok(alt(repo, &["commit", "-m", both]));
    };
    ok(alt(repo, &["init"]));
    commit("base\n", "1", "9");
    ok(alt(repo, &["switch", "-c", "theirs"]));
    commit("theirs\n", "one", "9");
    ok(alt(repo, &["switch", "main"]));
    ok(alt(repo, &["switch", "-c", "ours"]));
    commit("ours\n", "1", "nine");

    let (alt_dir, coord) = resolve_workspace(repo, None).unwrap();
    let mut store = Store::open(alt_dir).unwrap();
    store.set_defer_durability(true);
    let mut merging = NativeRepo::attach(&mut store, coord, Identity::from_env(), None);
    let stopped = merging.merge("theirs", false, &mut Vec::new()).unwrap();
    assert!(stopped, "the merge did not stop on the conflict");
    let merged = std::fs::read_to_string(repo.join("each.txt")).unwrap();
    assert_eq!(merged, lines("one", "nine"));
    assert!(
        store.odb().confirmed().unwrap(),
        "the index was written before the blobs it names were synced"
    );
}
