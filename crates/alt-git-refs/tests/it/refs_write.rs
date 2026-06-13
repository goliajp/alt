//! Loose-ref writing: write from scratch, read back with our own
//! (git-verified) reader, and check the on-disk shapes git expects.

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_git_refs::{RefStore, RefTarget, write_loose};
use bstr::BString;

fn oid(n: u8) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &[n])
}

#[test]
fn written_refs_read_back_identically() {
    let dir = tempfile::tempdir().unwrap();
    let refs: Vec<(BString, RefTarget)> = vec![
        ("refs/heads/main".into(), RefTarget::Direct(oid(1))),
        (
            "refs/heads/feat/nested/deep".into(),
            RefTarget::Direct(oid(2)),
        ),
        ("refs/tags/v1".into(), RefTarget::Direct(oid(3))),
        (
            "refs/remotes/origin/HEAD".into(),
            RefTarget::Symbolic("refs/remotes/origin/main".into()),
        ),
    ];
    let head = RefTarget::Symbolic("refs/heads/main".into());
    write_loose(dir.path(), Some(&head), &refs).unwrap();

    let store = RefStore::open(dir.path(), HashAlgo::Sha1).unwrap();
    assert_eq!(store.read("HEAD").unwrap(), Some(head));
    for (name, target) in &refs {
        let name = std::str::from_utf8(name).unwrap();
        assert_eq!(store.read(name).unwrap().as_ref(), Some(target), "{name}");
    }
    // exact on-disk shapes
    let head_bytes = std::fs::read(dir.path().join("HEAD")).unwrap();
    assert_eq!(head_bytes, b"ref: refs/heads/main\n");
    let main_bytes = std::fs::read(dir.path().join("refs/heads/main")).unwrap();
    assert_eq!(main_bytes, format!("{}\n", oid(1)).into_bytes());
}

#[test]
fn detached_head_is_a_bare_oid_line() {
    let dir = tempfile::tempdir().unwrap();
    write_loose(dir.path(), Some(&RefTarget::Direct(oid(7))), &[]).unwrap();
    let head_bytes = std::fs::read(dir.path().join("HEAD")).unwrap();
    assert_eq!(head_bytes, format!("{}\n", oid(7)).into_bytes());
}

#[test]
fn path_shaped_ref_names_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    for bad in ["refs/../escape", "/abs", "refs//double", "refs/x/", ""] {
        let refs = vec![(BString::from(bad), RefTarget::Direct(oid(1)))];
        assert!(
            write_loose(dir.path(), None, &refs).is_err(),
            "{bad:?} must be rejected"
        );
    }
}
