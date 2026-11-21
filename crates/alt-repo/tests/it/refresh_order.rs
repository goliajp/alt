//! A held handle refreshes its refs before its object store: a writer
//! stores its objects before the ref that names them, so refs read first
//! never name an object the refreshed store lacks.

use std::fs::OpenOptions;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_git_codec::{ObjectId, ObjectKind};
use alt_odb::NativeOdb;
use alt_refs::{RefChange, RefStore, RefTarget};
use alt_repo::Repository;

/// `map.alt` is a header, then records of one size, each ending in its
/// checksum.
const HEADER: u64 = 5;
const RECORD: u64 = 82;

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

/// The object store is damaged past what the handle has read, so that part
/// of the refresh fails: the refs having moved by then shows that they
/// were read first.
#[test]
fn refresh_reads_the_refs_before_the_object_store() {
    let repo_dir = alt_os::temp::tempdir().unwrap();
    alt_testutil::make_repo(repo_dir.path(), "sha1");
    let alt_root = alt_os::temp::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");
    let git = Repository::discover(repo_dir.path()).unwrap();
    alt_import::import_git(&git, &alt_dir, "test/native", 1).unwrap();
    let head = git.rev_parse("HEAD").unwrap().unwrap();

    // this handle has both its refs and its object store open
    let mut repo = Repository::discover(alt_root.path()).unwrap();
    assert_eq!(repo.resolve_ref("HEAD").unwrap(), Some(head));
    let commit = repo.read_object(&head).unwrap().unwrap();

    // another process commits: its objects, then the ref that names one
    let mut data = commit.data.clone();
    data.extend_from_slice(b"amended\n");
    let amended = ObjectId::hash_object(head.algo(), ObjectKind::Commit, &data);
    let spare = b"stored along with the commit\n";
    let mut odb = NativeOdb::open(&alt_dir).unwrap();
    odb.put(amended, ObjectKind::Commit, &data).unwrap();
    let spare_oid = ObjectId::hash_object(head.algo(), ObjectKind::Blob, spare);
    odb.put(spare_oid, ObjectKind::Blob, spare).unwrap();
    odb.flush().unwrap();
    drop(odb);
    let mut refs = RefStore::open(&alt_dir).unwrap();
    let Some(RefTarget::Symbolic(branch)) = refs.get("HEAD").cloned() else {
        panic!("HEAD is on a branch");
    };
    let moved = RefChange {
        name: branch,
        old: Some(RefTarget::Oid(head)),
        new: Some(RefTarget::Oid(amended)),
    };
    refs.commit("test", 0, &[moved]).unwrap();
    drop(refs);
    damage_map(&alt_dir);

    let failed = repo
        .refresh()
        .expect_err("a damaged record fails the refresh of the object store");
    assert!(format!("{failed:?}").contains("map.alt"), "{failed:?}");
    assert_eq!(
        repo.resolve_ref("HEAD").unwrap(),
        Some(amended),
        "the object store was refreshed before the refs"
    );
}
