//! `alt fsck` walks from every ref through commits and trees and names
//! each object they lead to that the store does not hold.

use std::fs;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_refs::{RefChange, RefStore, RefTarget};

use super::staged_durability::repo;
use super::unconfirmed::{alt, ok, stderr};

fn oid_of(kind: ObjectKind, data: &[u8]) -> ObjectId {
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
fn missing(dir: &Path) -> String {
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
    let entry = [b"100644 b.txt\0", blob.as_bytes()].concat();
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
    let link = [b"160000 linked\0", parent.as_bytes()].concat();
    let older = an_older_commit_names(dir, &link, &[parent]);
    let said = missing(dir);
    let line = format!("missing commit {parent}: commit {older}, reached from refs/heads/");
    assert!(said.contains(&line), "{said}");
    assert!(said.contains(", has it as a parent\n"), "{said}");
    assert_eq!(said.matches("missing ").count(), 1, "{said}");
}
