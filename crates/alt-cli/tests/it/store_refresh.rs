//! A held store refreshes its refs before its object store: a writer
//! stores its objects before the ref that names them, so refs read first
//! never name an object the refreshed store lacks.

use std::fs::OpenOptions;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_cli::native::Store;
use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;
use alt_refs::{RefChange, RefStore, RefTarget};

const BRANCH: &str = "refs/heads/main";

/// `map.alt` is a header, then records of one size, each ending in its
/// checksum.
const HEADER: u64 = 5;
const RECORD: u64 = 82;

/// What a writer does: its objects, then the ref that names the first.
fn commit(alt_dir: &Path, old: Option<ObjectId>, texts: [&str; 2]) -> ObjectId {
    let oids =
        texts.map(|text| ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, text.as_bytes()));
    let mut odb = NativeOdb::open(alt_dir).unwrap();
    for (oid, text) in oids.iter().zip(texts) {
        odb.put(*oid, ObjectKind::Blob, text.as_bytes()).unwrap();
    }
    odb.flush().unwrap();
    let mut refs = RefStore::open(alt_dir).unwrap();
    let moved = RefChange {
        name: BRANCH.to_owned(),
        old: old.map(RefTarget::Oid),
        new: Some(RefTarget::Oid(oids[0])),
    };
    refs.commit("test", 0, &[moved]).unwrap();
    oids[0]
}

/// Damages the record before the last one of `map.alt`: not an append cut
/// short, which would be the last.
fn damage_map(alt_dir: &Path) {
    let map = OpenOptions::new()
        .read(true)
        .write(true)
        .open(alt_dir.join("map.alt"))
        .unwrap();
    let len = map.metadata().unwrap().len();
    assert_eq!((len - HEADER) % RECORD, 0, "the size of a record");
    let at = len - RECORD - 1;
    let mut byte = [0u8];
    map.read_exact_at(&mut byte, at).unwrap();
    map.write_all_at(&[byte[0] ^ 1], at).unwrap();
}

/// The object store is damaged past what the store has read, so that part
/// of the refresh fails: the refs having moved by then shows that they
/// were read first.
#[test]
fn refresh_reads_the_refs_before_the_object_store() {
    let root = alt_os::temp::tempdir().unwrap();
    let alt_dir = root.path().join(".alt");
    let first = commit(&alt_dir, None, ["first\n", "its spare\n"]);

    let mut store = Store::open(alt_dir.clone()).unwrap();
    // the object store opens with the first read
    assert!(store.odb_get(&first).unwrap().is_some());
    assert_eq!(store.refs_mut().resolve(BRANCH).unwrap(), Some(first));

    let second = commit(&alt_dir, Some(first), ["second\n", "another spare\n"]);
    damage_map(&alt_dir);

    let failed = store
        .refresh()
        .expect_err("a damaged record fails the refresh of the object store");
    assert!(format!("{failed:?}").contains("map.alt"), "{failed:?}");
    assert_eq!(
        store.refs_mut().resolve(BRANCH).unwrap(),
        Some(second),
        "the object store was refreshed before the refs"
    );
}
