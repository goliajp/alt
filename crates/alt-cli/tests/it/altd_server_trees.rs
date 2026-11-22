//! `altd-server` receiving commits whose trees are out of the ordinary:
//! one with a gitlink, and the empty tree, which Git reads in any
//! repository and the odb here holds like any other object.

use std::path::Path;
use std::process::Command;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;

use crate::altd_server::{alt, ok, spawn_server};
use crate::altd_server_no_new_objects::{creation, git, push, seeded, tip};
use crate::altd_server_signed_commits::{
    accepted, commit_object, pack_of, pack_of_one, refused_over,
};

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

fn holds(origin: &Path, oid: &ObjectId) -> bool {
    let odb = NativeOdb::open(origin.join(".alt")).unwrap();
    odb.contains(oid).unwrap()
}

/// A gitlink names a commit of another repository, which the push does
/// not bring and the server does not ask for.
#[test]
fn a_commit_whose_tree_has_a_gitlink_is_accepted() {
    let (origin, seed) = seeded();
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let elsewhere = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, b"elsewhere");
    let mut tree = b"160000 sub\0".to_vec();
    tree.extend_from_slice(elsewhere.as_bytes());
    let tree_oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &tree);
    let who = "tester <t@e> 1700000000 +0000";
    let commit = format!("tree {tree_oid}\nparent {seed}\nauthor {who}\ncommitter {who}\n\nsub\n");
    let commit = commit.into_bytes();
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &commit);
    let scratch = alt_os::temp::tempdir().unwrap();
    let mut writer = alt_git_pack::PackWriter::create(scratch.path(), HashAlgo::Sha1, 2).unwrap();
    writer.add(tree_oid, ObjectKind::Tree, &tree).unwrap();
    writer.add(oid, ObjectKind::Commit, &commit).unwrap();
    let pack = std::fs::read(writer.finish().unwrap().pack_path).unwrap();

    let name = oid.to_string();
    let report = push(&url, &[creation("refs/heads/sub", &name)], &pack);
    assert!(accepted(&report), "{report:?}");
    drop(server);
    assert_eq!(tip(origin.path(), "refs/heads/sub"), name);
    assert!(holds(origin.path(), &tree_oid));
    assert!(!holds(origin.path(), &elsewhere));
    ok(alt(origin.path(), &["fsck"]));
}

/// Git reads the empty tree in a repository that never stored it, and
/// puts it in the pack of a push all the same: the server, which reads
/// it only once it is stored, is sent it.
#[test]
fn git_sends_the_empty_tree_it_never_stored() {
    let (origin, seed) = seeded();
    let empty: ObjectId = EMPTY_TREE.parse().unwrap();
    assert!(!holds(origin.path(), &empty));
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let work = alt_os::temp::tempdir().unwrap();
    git(work.path(), &["clone", "-q", &url, "clone"]);
    let clone = work.path().join("clone");
    let made = Command::new("git")
        .current_dir(&clone)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .args(["commit-tree", EMPTY_TREE, "-p", &seed, "-m", "empty"])
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    let commit = String::from_utf8(made.stdout).unwrap().trim().to_owned();
    let stored = clone.join(".git/objects/4b").join(&EMPTY_TREE[2..]);
    assert!(!stored.exists(), "the clone stored the empty tree");
    let to = format!("{commit}:refs/heads/empty");
    git(&clone, &["push", "-q", "origin", &to]);
    drop(server);
    assert_eq!(tip(origin.path(), "refs/heads/empty"), commit);
    assert!(holds(origin.path(), &empty));
    ok(alt(origin.path(), &["fsck"]));
}

/// A pack that leaves the empty tree out is short of an object where the
/// server does not hold it, and whole where it does.
#[test]
fn a_commit_of_the_empty_tree_needs_the_tree_like_any_other() {
    let (origin, seed) = seeded();
    let empty: ObjectId = EMPTY_TREE.parse().unwrap();
    assert!(!holds(origin.path(), &empty));
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let (oid, commit) = commit_object(&seed, 1_700_000_000, "", "bare");
    let (_, bare) = pack_of_one(ObjectKind::Commit, &commit);
    let name = oid.to_string();
    let report = push(&url, &[creation("refs/heads/bare", &name)], &bare);
    assert!(
        refused_over(&report, "missing necessary objects"),
        "{report:?}"
    );
    assert!(!holds(origin.path(), &oid));

    // another commit brings the tree along
    let with = commit_object(&seed, 1_700_000_000, "", "with the tree");
    let with_name = with.0.to_string();
    let report = push(
        &url,
        &[creation("refs/heads/with", &with_name)],
        &pack_of(&[with]),
    );
    assert!(accepted(&report), "{report:?}");
    let report = push(&url, &[creation("refs/heads/bare", &name)], &bare);
    assert!(accepted(&report), "{report:?}");
    drop(server);
    assert_eq!(tip(origin.path(), "refs/heads/bare"), name);
    ok(alt(origin.path(), &["fsck"]));
}
