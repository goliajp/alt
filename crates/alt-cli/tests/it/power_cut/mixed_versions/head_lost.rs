//! A blob that the commit at HEAD names and the index has as HEAD has
//! it, taken out of the store with `--committed`: nothing is staged over
//! HEAD at its path, and an add of the whole tree still stores it again.

use std::fs;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_refs::{RefChange, RefStore, RefTarget};

use super::super::super::mixed_versions::{new_bin, old_bin, run};
use super::super::super::staged_durability::repo;
use super::{an_older_build_wrote_past_a_lost_blob, ok};

fn oid_of(kind: ObjectKind, data: &[u8]) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, kind, data)
}

/// Makes the branch HEAD is on point at a commit on top of it whose
/// tree has `b.txt` as well, with the blob it is about to be staged as,
/// which is not stored yet.
fn head_names_the_blob_to_come(dir: &Path) {
    let new = new_bin();
    let head = ok(run(&new, dir, &["rev-parse", "HEAD"]));
    let head = ObjectId::from_hex(head.trim().as_bytes()).unwrap();
    let mut odb = alt_odb::NativeOdb::open(dir.join(".alt")).unwrap();
    let commit = odb.get(&head).unwrap().unwrap();
    let commit = alt_git_codec::Commit::parse(&commit.data).unwrap();
    let mut tree = odb.get(&commit.tree().unwrap()).unwrap().unwrap().data;
    // after `a.txt`, the first entry: its mode, its name, a zero, its id
    let entry = [
        b"100644 b.txt\0",
        oid_of(ObjectKind::Blob, b"b.txt\n").as_bytes(),
    ]
    .concat();
    let at = b"100644 a.txt\0".len() + 20;
    tree.splice(at..at, entry);
    let tree_oid = oid_of(ObjectKind::Tree, &tree);
    let who = "t <t@e> 0 +0000";
    let next = format!("tree {tree_oid}\nparent {head}\nauthor {who}\ncommitter {who}\n\nnext\n");
    let next_oid = oid_of(ObjectKind::Commit, next.as_bytes());
    odb.put(tree_oid, ObjectKind::Tree, &tree).unwrap();
    odb.put(next_oid, ObjectKind::Commit, next.as_bytes())
        .unwrap();
    odb.flush().unwrap();
    drop(odb);
    let status = ok(run(&new, dir, &["status"]));
    let branch = status.lines().next().unwrap().strip_prefix("On branch ");
    let moved = RefChange {
        name: format!("refs/heads/{}", branch.unwrap()),
        old: Some(RefTarget::Oid(head)),
        new: Some(RefTarget::Oid(next_oid)),
    };
    let mut refs = RefStore::open(dir.join(".alt")).unwrap();
    refs.commit("test", 0, &[moved]).unwrap();
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_blob_of_head_taken_out_with_its_commit_is_stored_again_by_an_add_of_the_whole_tree() {
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    // enough tracked files for an add of the tree to look at what is
    // staged over HEAD instead of asking the store about each entry
    for n in 0..40 {
        fs::write(dir.join(format!("e{n}.txt")), format!("e{n}\n")).unwrap();
    }
    ok(run(&new, dir, &["add", "."]));
    ok(run(&new, dir, &["commit", "-m", "many"]));
    head_names_the_blob_to_come(dir);
    an_older_build_wrote_past_a_lost_blob(&old, dir);
    let listed = run(&new, dir, &["fsck", "--drop-lost", "--yes", "--committed"]);
    let taken = ok(listed);
    assert!(
        taken.contains("names") && taken.contains("at b.txt"),
        "{taken}"
    );
    assert!(taken.contains("took 1 record(s) out"), "{taken}");
    let lost = fs::read_to_string(dir.join(".alt/objects.lost")).unwrap();
    assert_eq!(lost, format!("{}\n", oid_of(ObjectKind::Blob, b"b.txt\n")));
    // HEAD and the index have b.txt alike, and its blob does not read
    let status = ok(run(&new, dir, &["status"]));
    assert!(!status.contains("b.txt"), "{status}");
    assert!(!run(&new, dir, &["show", "HEAD:b.txt"]).status.success());
    ok(run(&new, dir, &["add", "."]));
    assert_eq!(ok(run(&new, dir, &["show", "HEAD:b.txt"])), "b.txt\n");
    ok(run(&new, dir, &["commit", "-m", "second"]));
    assert!(ok(run(&new, dir, &["fsck"])).ends_with("every record verified\n"));
    assert_eq!(ok(run(&old, dir, &["show", "HEAD:b.txt"])), "b.txt\n");
    // the next add that looks finds every object of the list stored
    // again, and the list goes
    ok(run(&new, dir, &["add", "."]));
    assert!(!dir.join(".alt/objects.lost").exists());
}
