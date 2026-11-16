//! A handle that reads an object before it reads a ref: whatever a ref
//! then names is an object the handle can read, though another process
//! committed in between.

use alt_git_codec::{ObjectId, ObjectKind};
use alt_odb::NativeOdb;
use alt_refs::{RefChange, RefStore, RefTarget};
use alt_repo::Repository;

#[test]
fn a_ref_read_after_an_object_names_an_object_the_handle_holds() {
    let repo_dir = alt_os::temp::tempdir().unwrap();
    alt_testutil::make_repo(repo_dir.path(), "sha1");
    let alt_root = alt_os::temp::tempdir().unwrap();
    let alt_dir = alt_root.path().join(".alt");
    let git = Repository::discover(repo_dir.path()).unwrap();
    alt_import::import_git(&git, &alt_dir, "test/native", 1).unwrap();
    let head = git.rev_parse("HEAD").unwrap().unwrap();

    // this handle reads an object by id first
    let repo = Repository::discover(alt_root.path()).unwrap();
    let commit = repo.read_object(&head).unwrap().unwrap();

    // another process commits: the object, then the ref that names it
    let mut data = commit.data.clone();
    data.extend_from_slice(b"amended\n");
    let amended = ObjectId::hash_object(head.algo(), ObjectKind::Commit, &data);
    let mut odb = NativeOdb::open(&alt_dir).unwrap();
    odb.put(amended, ObjectKind::Commit, &data).unwrap();
    odb.flush().unwrap();
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

    let tip = repo.rev_parse("HEAD").unwrap().unwrap();
    assert!(
        repo.read_object(&tip).unwrap().is_some(),
        "HEAD names {tip}, which this handle cannot read"
    );
}
