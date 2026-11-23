//! `altd-server` letting a ref move only to history it holds whole: every
//! commit, tree and blob a push adds is in the odb or in its pack, as
//! Git's `receive-pack` has it. Without that a commit could be brought in
//! under history that lacked it, by a push that never had it looked at.

use std::cell::Cell;
use std::fs::File;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;
use alt_refs::{RefChange, RefStore, RefTarget};

use crate::altd_server::{alt, ok, spawn_server};
use crate::altd_server_added_commits::odb_state;
use crate::altd_server_no_new_objects::{creation, push, seeded, tip};
use crate::altd_server_signed_commits::{
    accepted, commit_object, refused_over, require_signed_commits,
};

type Object = (ObjectId, ObjectKind, Vec<u8>);

fn object(kind: ObjectKind, data: &[u8]) -> Object {
    let oid = ObjectId::hash_object(HashAlgo::Sha1, kind, data);
    (oid, kind, data.to_vec())
}

fn empty_tree() -> Object {
    object(ObjectKind::Tree, b"")
}

fn commit(parent: &str, message: &str) -> Object {
    let (oid, bytes) = commit_object(parent, 1_700_000_000, "", message);
    (oid, ObjectKind::Commit, bytes)
}

/// A commit on top of `parent` that `secret` signed as `alice`.
fn signed(secret: &alt_sign::SecretKey, parent: &str, message: &str) -> Object {
    let (_, plain) = commit_object(parent, 1_700_000_000, "", message);
    let line = alt_cli::commit_sign::alt_sig_line("alice", &secret.sign(&plain));
    let bytes = alt_cli::commit_sign::embed_alt_sig(&plain, &line).unwrap();
    object(ObjectKind::Commit, &bytes)
}

/// A commit of `tree` on top of `parent`.
fn commit_of(tree: &ObjectId, parent: &str) -> Object {
    let who = "tester <t@e> 1700000000 +0000";
    let text = format!("tree {tree}\nparent {parent}\nauthor {who}\ncommitter {who}\n\ntree\n");
    object(ObjectKind::Commit, text.as_bytes())
}

/// A tree of one entry.
fn tree_of(name: &str, dir: bool, oid: &ObjectId) -> Object {
    let mode = if dir { "40000" } else { "100644" };
    let mut data = format!("{mode} {name}\0").into_bytes();
    data.extend_from_slice(oid.as_bytes());
    object(ObjectKind::Tree, &data)
}

fn pack(objects: &[&Object]) -> Vec<u8> {
    let scratch = alt_os::temp::tempdir().unwrap();
    let count = objects.len() as u32;
    let mut writer =
        alt_git_pack::PackWriter::create(scratch.path(), HashAlgo::Sha1, count).unwrap();
    for (oid, kind, data) in objects {
        writer.add(*oid, *kind, data).unwrap();
    }
    std::fs::read(writer.finish().unwrap().pack_path).unwrap()
}

/// Has the served repository trust `alice`; her key.
fn trusted(origin: &Path) -> alt_sign::SecretKey {
    let (secret, public) = alt_sign::SecretKey::generate();
    let trust = origin.join(".alt").join("trust");
    std::fs::create_dir_all(&trust).unwrap();
    std::fs::write(trust.join("alice.pub"), public.to_text()).unwrap();
    secret
}

fn holds(origin: &Path, oid: &ObjectId) -> bool {
    let odb = NativeOdb::open(origin.join(".alt")).unwrap();
    odb.contains(oid).unwrap()
}

fn missing(report: &alt_wire::ReportStatus) -> bool {
    refused_over(report, "missing necessary objects")
}

/// A signed commit is pushed without its unsigned parent, then the parent
/// under a second signed commit. The first push is refused for the parent
/// it lacks, the second for the parent it adds.
#[test]
fn an_unsigned_commit_cannot_be_brought_in_under_a_signed_one_in_two_pushes() {
    let (origin, seed) = seeded();
    require_signed_commits(origin.path());
    let secret = trusted(origin.path());
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let before = odb_state(&origin.path().join(".alt"));
    let unsigned = commit(&seed, "unsigned");
    let above = signed(&secret, &unsigned.0.to_string(), "above");
    let top = signed(&secret, &above.0.to_string(), "top");
    let tree = empty_tree();

    let first = [creation("refs/heads/above", &above.0.to_string())];
    let report = push(&url, &first, &pack(&[&tree, &above]));
    assert!(missing(&report), "{report:?}");
    let second = [creation("refs/heads/top", &top.0.to_string())];
    let report = push(&url, &second, &pack(&[&tree, &unsigned, &top]));
    assert!(missing(&report), "{report:?}");
    let report = push(&url, &second, &pack(&[&tree, &unsigned, &above, &top]));
    assert!(refused_over(&report, "missing alt-sig"), "{report:?}");
    drop(server);
    assert_eq!(odb_state(&origin.path().join(".alt")), before);
    let branches = ok(alt(origin.path(), &["branch"]));
    assert!(!branches.contains("above"), "{branches}");
    assert!(!branches.contains("top"), "{branches}");
}

/// The first of the two pushes comes from a pusher no policy holds to
/// anything: it is refused all the same, and so is a tag at a commit the
/// server lacks.
#[test]
fn a_commit_or_a_tag_above_a_commit_the_server_lacks_is_refused_for_any_pusher() {
    let (origin, seed) = seeded();
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let before = odb_state(&origin.path().join(".alt"));
    let absent = commit(&seed, "absent");
    let above = commit(&absent.0.to_string(), "above");
    let tree = empty_tree();
    let updates = [creation("refs/heads/above", &above.0.to_string())];
    let report = push(&url, &updates, &pack(&[&tree, &above]));
    assert!(missing(&report), "{report:?}");

    let tag = |target: &ObjectId| {
        let text =
            format!("object {target}\ntype commit\ntag t\ntagger tester <t@e> 1 +0000\n\nt\n");
        object(ObjectKind::Tag, text.as_bytes())
    };
    for target in [&absent.0, &above.0] {
        let tag = tag(target);
        let updates = [creation("refs/tags/t", &tag.0.to_string())];
        let report = push(&url, &updates, &pack(&[&tree, &above, &tag]));
        assert!(missing(&report), "{report:?}");
    }
    // with the commit, both are let in
    let tag = tag(&above.0);
    let updates = [
        creation("refs/heads/above", &above.0.to_string()),
        creation("refs/tags/t", &tag.0.to_string()),
    ];
    assert_eq!(odb_state(&origin.path().join(".alt")), before);
    let report = push(&url, &updates, &pack(&[&tree, &absent, &above, &tag]));
    assert_eq!(report.commands.len(), 2, "{report:?}");
    let ok_status =
        |status: &alt_wire::CommandStatus| matches!(status, alt_wire::CommandStatus::Ok(_));
    assert!(report.commands.iter().all(ok_status), "{report:?}");
    drop(server);
    assert_eq!(tip(origin.path(), "refs/heads/above"), above.0.to_string());
}

#[test]
fn a_commit_whose_tree_lacks_a_tree_or_a_blob_is_refused() {
    let (origin, seed) = seeded();
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let before = odb_state(&origin.path().join(".alt"));
    let blob = object(ObjectKind::Blob, b"contents\n");
    let low = tree_of("file", false, &blob.0);
    let root = tree_of("dir", true, &low.0);
    let new = commit_of(&root.0, &seed);
    let updates = [creation("refs/heads/new", &new.0.to_string())];
    for lacking in [
        vec![&new],
        vec![&new, &root],
        vec![&new, &root, &low],
        vec![&new, &root, &blob],
    ] {
        let report = push(&url, &updates, &pack(&lacking));
        assert!(missing(&report), "{} objects: {report:?}", lacking.len());
    }
    assert_eq!(odb_state(&origin.path().join(".alt")), before);
    let report = push(&url, &updates, &pack(&[&new, &root, &low, &blob]));
    assert!(accepted(&report), "{report:?}");
    drop(server);
    ok(alt(origin.path(), &["fsck"]));
    let contents = ok(alt(origin.path(), &["cat-file", "-p", &blob.0.to_string()]));
    assert_eq!(contents, "contents\n");
}

/// History the refs reach that lacks a commit, from before pushes were
/// looked at whole: put there by hand. A push on top of it is let in,
/// and the commit, unsigned, sent along with it, is not kept. Nor is any
/// other object of a pack that no new value reaches.
#[test]
fn of_a_pack_only_what_the_push_adds_is_kept() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let secret = trusted(origin.path());
    let lost = commit(&seed, "lost");
    let served = signed(&secret, &lost.0.to_string(), "served");
    let mut odb = NativeOdb::open(&alt_dir).unwrap();
    for (oid, kind, data) in [&empty_tree(), &served] {
        odb.put(*oid, *kind, data).unwrap();
    }
    odb.flush().unwrap();
    drop(odb);
    let at = RefChange {
        name: "refs/heads/served".into(),
        old: None,
        new: Some(RefTarget::Oid(served.0)),
    };
    let mut refs = RefStore::open(&alt_dir).unwrap();
    refs.commit("by hand", 0, &[at]).unwrap();
    drop(refs);
    require_signed_commits(origin.path());
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);

    let top = signed(&secret, &served.0.to_string(), "top");
    let stray = commit(&seed, "stray");
    let blob = object(ObjectKind::Blob, b"stray\n");
    let updates = [creation("refs/heads/top", &top.0.to_string())];
    let report = push(&url, &updates, &pack(&[&lost, &stray, &blob, &top]));
    assert!(accepted(&report), "{report:?}");
    // what was not kept cannot be pointed at
    for oid in [&lost.0, &stray.0] {
        let updates = [creation("refs/heads/kept", &oid.to_string())];
        let report = push(&url, &updates, &[]);
        assert!(missing(&report), "{report:?}");
    }
    drop(server);
    assert_eq!(tip(origin.path(), "refs/heads/top"), top.0.to_string());
    assert!(holds(origin.path(), &top.0));
    for oid in [&lost.0, &stray.0, &blob.0] {
        assert!(!holds(origin.path(), oid), "{oid}");
    }
}

/// The pack `git pack-objects --revs` makes of `revs` in `repo`, indexed;
/// with deltas that name their base by offset, as Git sends them here.
fn git_pack(repo: &Path, revs: &str) -> (alt_os::temp::TempDir, alt_cli::receive::PreparedPack) {
    use std::io::Write;
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("incoming.pack");
    let mut git = std::process::Command::new("git")
        .current_dir(repo)
        .args([
            "pack-objects",
            "--revs",
            "--stdout",
            "-q",
            "--delta-base-offset",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(File::create(&path).unwrap())
        .spawn()
        .unwrap();
    git.stdin
        .take()
        .unwrap()
        .write_all(revs.as_bytes())
        .unwrap();
    assert!(git.wait().unwrap().success());
    let prepared = alt_cli::receive::prepare_pack(&path, &Default::default()).unwrap();
    (dir, prepared)
}

fn rev(repo: &Path, rev: &str) -> ObjectId {
    let out = std::process::Command::new("git")
        .current_dir(repo)
        .args(["rev-parse", rev])
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// How many objects are read, and how many are asked for, to tell what
/// a push of `new` adds where the refs point at `served`: `had` holds
/// what the server has, `pack` what the push brings.
fn looked_at(
    had: Option<&alt_git_pack::IndexedPack>,
    pack: &alt_git_pack::IndexedPack,
    served: &[ObjectId],
    new: ObjectId,
) -> (usize, usize, usize) {
    use alt_cli::added_commits::{Source, added};
    type Res<T> = Result<T, Box<dyn std::error::Error>>;
    let (reads, asked) = (Cell::new(0), Cell::new(0));
    let get = |oid: &ObjectId| -> Res<Option<alt_git_codec::RawObject>> {
        reads.set(reads.get() + 1);
        for from in had.into_iter().chain([pack]) {
            if let Some(object) = from.read(oid)? {
                return Ok(Some(object.to_raw()));
            }
        }
        Ok(None)
    };
    let in_had = |oid: &ObjectId| had.is_some_and(|had| had.idx().lookup(oid).is_some());
    let holds = |oid: &ObjectId| -> Res<bool> {
        asked.set(asked.get() + 1);
        Ok(in_had(oid) || pack.idx().lookup(oid).is_some())
    };
    let had = |oid: &ObjectId| -> Res<bool> { Ok(in_had(oid)) };
    let fails = |_: &ObjectId| -> Res<Option<String>> { Ok(None) };
    let source = Source {
        get: &get,
        holds: &holds,
        had: &had,
        fails: &fails,
    };
    let added = added(&source, served, &[new]).unwrap().unwrap();
    (reads.get(), asked.get(), added.len())
}

/// Not a check of anything but that the pushes are let in: prints what
/// looking at a push of cargo's history costs (run with `--nocapture`).
#[test]
#[ignore = "needs $ALT_CORPUS pointing at a directory of git repos"]
fn the_reads_of_looking_at_a_push_of_cargo() {
    let corpus = std::env::var("ALT_CORPUS").expect("set ALT_CORPUS to the corpus directory");
    let repo = Path::new(&corpus).join("cargo");
    let (head, base) = (rev(&repo, "HEAD"), rev(&repo, "HEAD~5"));
    let (_all, (all, _)) = git_pack(&repo, "HEAD\n");
    let (_under, (under, _)) = git_pack(&repo, "HEAD~5\n");
    let (_top, (top, _)) = git_pack(&repo, "HEAD\n^HEAD~5\n");

    let started = std::time::Instant::now();
    let (reads, asked, added) = looked_at(None, &all, &[], head);
    eprintln!(
        "first push of all history: {} objects in the pack, {added} added, \
         {reads} read, {asked} blobs asked for, {:?}",
        all.idx().len(),
        started.elapsed()
    );
    assert_eq!(added, all.idx().len() as usize);

    let started = std::time::Instant::now();
    let (reads, asked, added) = looked_at(Some(&under), &top, &[base], head);
    eprintln!(
        "push of the last 5 commits: {} objects in the pack, {added} added, \
         {reads} read, {asked} blobs asked for, {:?}",
        top.idx().len(),
        started.elapsed()
    );
    assert_eq!(added, top.idx().len() as usize);
}
