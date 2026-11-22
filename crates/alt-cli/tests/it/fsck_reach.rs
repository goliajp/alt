//! `alt fsck` walks from every ref through commits and trees and names
//! each object they lead to that the store does not hold.

use std::fs;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_refs::{RefChange, RefStore, RefTarget};

use super::staged_durability::repo;
use super::unconfirmed::{alt, ok, stderr};

pub(super) fn oid_of(kind: ObjectKind, data: &[u8]) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, kind, data)
}

/// Puts two commits on top of HEAD and moves its branch to the second:
/// an older one whose tree has `entry` after `a.txt` and whose parents
/// are HEAD and `parents`, and one with the tree of HEAD on top of it.
/// Nothing `entry` or `parents` name is stored. The id of the older one.
fn an_older_commit_names(dir: &Path, entry: &[u8], parents: &[ObjectId]) -> ObjectId {
    let head = ok(alt(dir, &["rev-parse", "HEAD"]));
    let head = ObjectId::from_hex(head.trim().as_bytes()).unwrap();
    let mut odb = alt_odb::NativeOdb::open(dir.join(".alt")).unwrap();
    let commit = odb.get(&head).unwrap().unwrap();
    let kept = alt_git_codec::Commit::parse(&commit.data)
        .unwrap()
        .tree()
        .unwrap();
    let mut tree = odb.get(&kept).unwrap().unwrap().data;
    tree.extend_from_slice(entry);
    let tree_oid = oid_of(ObjectKind::Tree, &tree);
    let who = "t <t@e> 0 +0000";
    let more: String = parents.iter().map(|p| format!("parent {p}\n")).collect();
    let older =
        format!("tree {tree_oid}\nparent {head}\n{more}author {who}\ncommitter {who}\n\nolder\n");
    let older_oid = oid_of(ObjectKind::Commit, older.as_bytes());
    let newer =
        format!("tree {kept}\nparent {older_oid}\nauthor {who}\ncommitter {who}\n\nnewer\n");
    let newer_oid = oid_of(ObjectKind::Commit, newer.as_bytes());
    odb.put(tree_oid, ObjectKind::Tree, &tree).unwrap();
    odb.put(older_oid, ObjectKind::Commit, older.as_bytes())
        .unwrap();
    odb.put(newer_oid, ObjectKind::Commit, newer.as_bytes())
        .unwrap();
    odb.flush().unwrap();
    drop(odb);
    let status = ok(alt(dir, &["status"]));
    let branch = status.lines().next().unwrap().strip_prefix("On branch ");
    let moved = RefChange {
        name: format!("refs/heads/{}", branch.unwrap()),
        old: Some(RefTarget::Oid(head)),
        new: Some(RefTarget::Oid(newer_oid)),
    };
    let mut refs = RefStore::open(dir.join(".alt")).unwrap();
    refs.commit("test", 0, &[moved]).unwrap();
    older_oid
}

/// What `alt fsck` fails with, after `fsck --no-connectivity` passed and
/// both left every file under `.alt` as it was.
pub(super) fn missing(dir: &Path) -> String {
    let before = files(&dir.join(".alt"));
    let records = ok(alt(dir, &["fsck", "--no-connectivity"]));
    assert!(records.ends_with("every record verified\n"), "{records}");
    let checked = alt(dir, &["fsck"]);
    assert!(!checked.status.success());
    assert_eq!(files(&dir.join(".alt")), before);
    stderr(&checked)
}

/// Every file under `dir` with its content, but the locks.
fn files(dir: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut dirs = vec![dir.to_owned()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if !path.to_string_lossy().ends_with("lock") {
                let content = fs::read(&path).unwrap();
                found.push((path, content));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn a_blob_of_an_older_commit_that_is_not_stored_is_named_with_the_commit_and_the_path() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let blob = oid_of(ObjectKind::Blob, b"b.txt\n");
    // at two paths: an object is named once, at the first found
    let entry = [
        &b"100644 b.txt\0"[..],
        blob.as_bytes(),
        b"100644 c.txt\0",
        blob.as_bytes(),
    ]
    .concat();
    let older = an_older_commit_names(dir, &entry, &[]);
    let said = missing(dir);
    let line = format!("missing blob {blob}: commit {older}, reached from refs/heads/");
    assert!(said.contains(&line), "{said}");
    assert!(said.contains(", names it at b.txt\n"), "{said}");
    assert_eq!(said.matches("missing ").count(), 1, "{said}");
    // listed as taken out on request, it is still reported, and the
    // list is left where it is
    let list = dir.join(".alt/objects.lost");
    fs::write(&list, format!("{blob}\n")).unwrap();
    let said = missing(dir);
    assert!(said.contains(&line), "{said}");
    assert!(
        said.contains("names it at b.txt; its records were taken out with"),
        "{said}"
    );
    assert!(list.exists());
    // stored again, nothing is missing
    fs::write(dir.join("b.txt"), "b.txt\n").unwrap();
    ok(alt(dir, &["add", "b.txt"]));
    assert!(ok(alt(dir, &["fsck"])).ends_with("every record verified\n"));
}

#[test]
fn a_tree_of_an_older_commit_that_is_not_stored_is_named_with_the_commit_and_the_path() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let inner = [
        b"100644 c.txt\0",
        oid_of(ObjectKind::Blob, b"c\n").as_bytes(),
    ]
    .concat();
    let tree = oid_of(ObjectKind::Tree, &inner);
    let entry = [b"40000 sub\0", tree.as_bytes()].concat();
    let older = an_older_commit_names(dir, &entry, &[]);
    let said = missing(dir);
    let line = format!("missing tree {tree}: commit {older}, reached from refs/heads/");
    assert!(said.contains(&line), "{said}");
    assert!(said.contains(", names it at sub/\n"), "{said}");
    // what a tree that is not stored names is not known
    assert_eq!(said.matches("missing ").count(), 1, "{said}");
}

#[test]
fn a_commit_that_is_not_stored_is_named_with_the_commit_it_is_a_parent_of() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let parent = oid_of(ObjectKind::Commit, b"never stored\n");
    // a commit of another repository at a path is not one of this store
    let elsewhere = oid_of(ObjectKind::Commit, b"of another repository\n");
    let link = [b"160000 linked\0", elsewhere.as_bytes()].concat();
    let older = an_older_commit_names(dir, &link, &[parent]);
    let said = missing(dir);
    let line = format!("missing commit {parent}: commit {older}, reached from refs/heads/");
    assert!(said.contains(&line), "{said}");
    assert!(said.contains(", has it as a parent\n"), "{said}");
    assert_eq!(said.matches("missing ").count(), 1, "{said}");
}

#[test]
fn what_a_tree_below_other_trees_names_is_told_with_the_whole_path() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let blob = oid_of(ObjectKind::Blob, b"x\n");
    let gone = oid_of(ObjectKind::Tree, b"never stored");
    let deep = [
        &b"100644 x.txt\0"[..],
        blob.as_bytes(),
        b"40000 gone\0",
        gone.as_bytes(),
    ]
    .concat();
    let deep_oid = oid_of(ObjectKind::Tree, &deep);
    let sub = [&b"40000 deep\0"[..], deep_oid.as_bytes()].concat();
    let sub_oid = oid_of(ObjectKind::Tree, &sub);
    let mut odb = alt_odb::NativeOdb::open(dir.join(".alt")).unwrap();
    odb.put(deep_oid, ObjectKind::Tree, &deep).unwrap();
    odb.put(sub_oid, ObjectKind::Tree, &sub).unwrap();
    odb.flush().unwrap();
    drop(odb);
    let entry = [&b"40000 sub\0"[..], sub_oid.as_bytes()].concat();
    let older = an_older_commit_names(dir, &entry, &[]);
    let said = missing(dir);
    let by = format!(": commit {older}, reached from refs/heads/");
    assert!(said.contains(&format!("missing blob {blob}{by}")), "{said}");
    assert!(said.contains(", names it at sub/deep/x.txt\n"), "{said}");
    assert!(said.contains(&format!("missing tree {gone}{by}")), "{said}");
    assert!(said.contains(", names it at sub/deep/gone/\n"), "{said}");
    assert_eq!(said.matches("missing ").count(), 2, "{said}");
}

#[test]
fn what_a_tag_names_and_the_tree_of_a_commit_are_told_when_they_are_not_stored() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let who = "t <t@e> 0 +0000";
    let tree = oid_of(ObjectKind::Tree, b"never stored");
    let commit = format!("tree {tree}\nauthor {who}\ncommitter {who}\n\nno tree\n");
    let commit_oid = oid_of(ObjectKind::Commit, commit.as_bytes());
    let target = oid_of(ObjectKind::Commit, b"never stored\n");
    let tag = |object: ObjectId, name: &str| {
        let tag = format!("object {object}\ntype commit\ntag {name}\ntagger {who}\n\n{name}\n");
        (oid_of(ObjectKind::Tag, tag.as_bytes()), tag)
    };
    let (kept, kept_tag) = tag(commit_oid, "kept");
    let (lost, lost_tag) = tag(target, "lost");
    let mut odb = alt_odb::NativeOdb::open(dir.join(".alt")).unwrap();
    odb.put(commit_oid, ObjectKind::Commit, commit.as_bytes())
        .unwrap();
    odb.put(kept, ObjectKind::Tag, kept_tag.as_bytes()).unwrap();
    odb.put(lost, ObjectKind::Tag, lost_tag.as_bytes()).unwrap();
    odb.flush().unwrap();
    drop(odb);
    let named = |name: &str, oid| RefChange {
        name: format!("refs/tags/{name}"),
        old: None,
        new: Some(RefTarget::Oid(oid)),
    };
    let mut refs = RefStore::open(dir.join(".alt")).unwrap();
    refs.commit("test", 0, &[named("kept", kept), named("lost", lost)])
        .unwrap();
    drop(refs);
    let said = missing(dir);
    let line = format!(
        "missing tree {tree}: commit {commit_oid}, reached from refs/tags/kept, names it at /\n"
    );
    assert!(said.contains(&line), "{said}");
    let line =
        format!("missing object {target}: tag {lost}, reached from refs/tags/lost, names it\n");
    assert!(said.contains(&line), "{said}");
    assert_eq!(said.matches("missing ").count(), 2, "{said}");
}

/// Where each chunk record of a pack starts.
fn pack_records(pack: &[u8]) -> Vec<usize> {
    let (mut at, mut found) = (5usize, Vec::new());
    while at < pack.len() {
        found.push(at);
        let stored = u32::from_le_bytes(pack[at + 37..at + 41].try_into().unwrap());
        at += 41 + stored as usize;
    }
    found
}

#[test]
fn a_tree_that_does_not_hold_what_it_names_is_told_before_anything_is_walked() {
    use std::os::unix::fs::FileExt;
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let blob = oid_of(ObjectKind::Blob, b"b.txt\n");
    let entry = [&b"100644 b.txt\0"[..], blob.as_bytes()].concat();
    an_older_commit_names(dir, &entry, &[]);
    assert!(missing(dir).contains(&format!("missing blob {blob}")));
    // the tree that names the blob is the third record from the end: the
    // last byte of what it holds is changed
    let pack = dir.join(".alt/store/pack-00000001.altpack");
    let bytes = fs::read(&pack).unwrap();
    let records = pack_records(&bytes);
    let end = records[records.len() - 2] - 1;
    let file = fs::OpenOptions::new().write(true).open(&pack).unwrap();
    file.write_all_at(&[!bytes[end]], end as u64).unwrap();
    for command in [&["fsck"][..], &["fsck", "--no-connectivity"]] {
        let checked = alt(dir, command);
        assert!(!checked.status.success());
        let said = stderr(&checked);
        // the walk reads a tree without hashing it: it is not come to
        assert!(!said.contains("missing "), "{said}");
        assert!(said.contains("corrupt: hash mismatch"), "{said}");
    }
}
