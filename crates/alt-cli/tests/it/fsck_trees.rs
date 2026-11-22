//! `alt fsck` walks a tree that a ref or a tag names, as it walks the
//! tree of a commit, and names what it leads to that is not stored.

use std::path::Path;

use alt_git_codec::{ObjectId, ObjectKind};
use alt_refs::{RefChange, RefStore, RefTarget};

use super::fsck_reach::{missing, oid_of};
use super::staged_durability::repo;

/// Stores each of `objects` and makes the ref `name` point at `oid`.
pub(super) fn named(dir: &Path, name: &str, oid: ObjectId, objects: &[(ObjectKind, &[u8])]) {
    let mut odb = alt_odb::NativeOdb::open(dir.join(".alt")).unwrap();
    for (kind, data) in objects {
        odb.put(oid_of(*kind, data), *kind, data).unwrap();
    }
    odb.flush().unwrap();
    drop(odb);
    let points = RefChange {
        name: name.to_owned(),
        old: None,
        new: Some(RefTarget::Oid(oid)),
    };
    let mut refs = RefStore::open(dir.join(".alt")).unwrap();
    refs.commit("test", 0, &[points]).unwrap();
}

/// A tag object that names the tree `tree`.
pub(super) fn tag_of(tree: ObjectId) -> Vec<u8> {
    format!("object {tree}\ntype tree\ntag of-a-tree\ntagger t <t@e> 0 +0000\n\nof a tree\n")
        .into_bytes()
}

#[test]
fn what_a_tree_that_a_tag_names_leads_to_is_told_when_it_is_not_stored() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let blob = oid_of(ObjectKind::Blob, b"b.txt\n");
    let gone = oid_of(ObjectKind::Tree, b"never stored");
    let tree = [
        &b"100644 b.txt\0"[..],
        blob.as_bytes(),
        b"40000 sub\0",
        gone.as_bytes(),
    ]
    .concat();
    let tree_oid = oid_of(ObjectKind::Tree, &tree);
    let tag = tag_of(tree_oid);
    let objects = [(ObjectKind::Tree, &tree[..]), (ObjectKind::Tag, &tag[..])];
    let tag_oid = oid_of(ObjectKind::Tag, &tag);
    named(dir, "refs/tags/of-a-tree", tag_oid, &objects);
    let said = missing(dir);
    let by = format!(": tree {tree_oid}, reached from refs/tags/of-a-tree, names it at ");
    assert!(
        said.contains(&format!("missing blob {blob}{by}b.txt\n")),
        "{said}"
    );
    assert!(
        said.contains(&format!("missing tree {gone}{by}sub/\n")),
        "{said}"
    );
    assert_eq!(said.matches("missing ").count(), 2, "{said}");
}

#[test]
fn what_a_tree_that_a_ref_points_at_leads_to_is_told_when_it_is_not_stored() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let blob = oid_of(ObjectKind::Blob, b"x\n");
    let deep = [&b"100644 x.txt\0"[..], blob.as_bytes()].concat();
    let deep_oid = oid_of(ObjectKind::Tree, &deep);
    let tree = [&b"40000 deep\0"[..], deep_oid.as_bytes()].concat();
    let tree_oid = oid_of(ObjectKind::Tree, &tree);
    let objects = [(ObjectKind::Tree, &deep[..]), (ObjectKind::Tree, &tree[..])];
    named(dir, "refs/kept/tree", tree_oid, &objects);
    let said = missing(dir);
    let line = format!(
        "missing blob {blob}: tree {tree_oid}, reached from refs/kept/tree, names it at deep/x.txt\n"
    );
    assert!(said.contains(&line), "{said}");
    assert_eq!(said.matches("missing ").count(), 1, "{said}");
}
