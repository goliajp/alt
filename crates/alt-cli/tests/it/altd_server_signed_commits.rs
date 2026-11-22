//! `altd-server` holding a pusher to `require-signed-commits`: every
//! commit the push adds to the history served, in its pack or already in
//! the odb, and none that the refs reached before.

use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_wire::CommandStatus;

use crate::altd_server::{alt, ok, spawn_server};
use crate::altd_server_no_new_objects::{creation, push, seeded, tip};

/// A commit of the empty tree on top of `parent`, committed at `date`,
/// with `header` (a whole line, or nothing) after its committer.
pub(crate) fn commit_object(
    parent: &str,
    date: u64,
    header: &str,
    message: &str,
) -> (ObjectId, Vec<u8>) {
    let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, b"");
    let who = format!("tester <t@e> {date} +0000");
    let commit = format!(
        "tree {tree}\nparent {parent}\nauthor {who}\ncommitter {who}\n{header}\n{message}\n"
    )
    .into_bytes();
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &commit),
        commit,
    )
}

/// The pack of `commits` and the empty tree they have.
pub(crate) fn pack_of(commits: &[(ObjectId, Vec<u8>)]) -> Vec<u8> {
    let scratch = alt_os::temp::tempdir().unwrap();
    let count = commits.len() as u32 + 1;
    let mut writer =
        alt_git_pack::PackWriter::create(scratch.path(), HashAlgo::Sha1, count).unwrap();
    let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, b"");
    writer.add(tree, ObjectKind::Tree, b"").unwrap();
    for (oid, commit) in commits {
        writer.add(*oid, ObjectKind::Commit, commit).unwrap();
    }
    let written = writer.finish().unwrap();
    std::fs::read(&written.pack_path).unwrap()
}

pub(crate) fn require_signed_commits(origin: &Path) {
    std::fs::write(
        origin.join(".alt").join("policy"),
        "human:anonymous -> require-signed-commits\n",
    )
    .unwrap();
}

pub(crate) fn refused_over(report: &alt_wire::ReportStatus, word: &str) -> bool {
    matches!(report.commands.as_slice(),
        [CommandStatus::Ng { reason, .. }] if reason.contains(word))
}

pub(crate) fn accepted(report: &alt_wire::ReportStatus) -> bool {
    matches!(report.commands.as_slice(), [CommandStatus::Ok(_)])
}

#[test]
fn a_commit_with_an_unreadable_signature_is_refused_and_not_kept() {
    let (origin, seed) = seeded();
    require_signed_commits(origin.path());
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let (oid, commit) = commit_object(&seed, 1_700_000_000, "alt-sig x\n", "unreadable");
    let name = oid.to_string();

    let report = push(
        &url,
        &[creation("refs/heads/with-pack", &name)],
        &pack_of(&[(oid, commit)]),
    );
    assert_eq!(report.unpack, Ok(()));
    assert!(refused_over(&report, "alt-sig"), "{report:?}");
    // the same pusher, pointing a ref at the commit without sending it
    let report = push(&url, &[creation("refs/heads/without-pack", &name)], &[]);
    assert!(
        refused_over(&report, "missing necessary objects"),
        "{report:?}"
    );
    drop(server);
    let branches = ok(alt(origin.path(), &["branch"]));
    assert!(!branches.contains("pack"), "{branches}");
}

#[test]
fn a_ref_created_without_objects_is_held_to_the_policy_for_what_it_adds() {
    let (origin, seed) = seeded();
    let root = origin.path();
    // a commit the odb holds and no ref reaches
    ok(alt(root, &["branch", "gone"]));
    ok(alt(root, &["switch", "gone"]));
    std::fs::write(root.join("more.txt"), "more\n").unwrap();
    ok(alt(root, &["add", "more.txt"]));
    ok(alt(root, &["commit", "-m", "unreferenced"]));
    let unreferenced = tip(root, "refs/heads/gone");
    ok(alt(root, &["switch", "main"]));
    ok(alt(root, &["branch", "-d", "gone"]));
    require_signed_commits(root);
    let server = spawn_server(root);
    let url = format!("http://{}/", server.addr);

    let report = push(&url, &[creation("refs/heads/added", &unreferenced)], &[]);
    assert!(refused_over(&report, "missing alt-sig"), "{report:?}");
    // the commit `main` is at has no signature either, and is served
    let report = push(&url, &[creation("refs/heads/served", &seed)], &[]);
    assert!(accepted(&report), "{report:?}");
    drop(server);
    assert_eq!(tip(root, "refs/heads/served"), seed);
    let branches = ok(alt(root, &["branch"]));
    assert!(!branches.contains("added"), "{branches}");
}

#[test]
fn a_served_commit_dated_after_its_child_is_not_taken_for_added() {
    let (origin, seed) = seeded();
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let (parent, parent_bytes) = commit_object(&seed, 1_800_000_000, "", "dated late");
    let (child, child_bytes) = commit_object(&parent.to_string(), 1_600_000_000, "", "child");
    let report = push(
        &url,
        &[creation("refs/heads/child", &child.to_string())],
        &pack_of(&[(parent, parent_bytes), (child, child_bytes)]),
    );
    assert!(accepted(&report), "{report:?}");

    // read anew by the server with the next request
    require_signed_commits(origin.path());
    let report = push(
        &url,
        &[creation("refs/heads/parent", &parent.to_string())],
        &[],
    );
    assert!(accepted(&report), "{report:?}");
    drop(server);
    assert_eq!(tip(origin.path(), "refs/heads/parent"), parent.to_string());
}

#[test]
fn signed_commits_are_accepted_and_an_unsigned_one_among_them_refuses_all() {
    let (origin, seed) = seeded();
    require_signed_commits(origin.path());
    let (secret, public) = alt_sign::SecretKey::generate();
    let trust = origin.path().join(".alt").join("trust");
    std::fs::create_dir_all(&trust).unwrap();
    std::fs::write(trust.join("alice.pub"), public.to_text()).unwrap();
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let signed = |parent: &str, message: &str| {
        let (_, plain) = commit_object(parent, 1_700_000_000, "", message);
        let line = alt_cli::commit_sign::alt_sig_line("alice", &secret.sign(&plain));
        let bytes = alt_cli::commit_sign::embed_alt_sig(&plain, &line).unwrap();
        (
            ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &bytes),
            bytes,
        )
    };

    let first = signed(&seed, "first");
    let second = signed(&first.0.to_string(), "second");
    let unsigned = commit_object(&second.0.to_string(), 1_700_000_001, "", "unsigned");
    let report = push(
        &url,
        &[creation("refs/heads/mixed", &unsigned.0.to_string())],
        &pack_of(&[first.clone(), second.clone(), unsigned]),
    );
    assert!(refused_over(&report, "missing alt-sig"), "{report:?}");
    let name = second.0.to_string();
    let report = push(
        &url,
        &[creation("refs/heads/signed", &name)],
        &pack_of(&[first, second]),
    );
    assert!(accepted(&report), "{report:?}");
    drop(server);
    assert_eq!(tip(origin.path(), "refs/heads/signed"), name);
    let branches = ok(alt(origin.path(), &["branch"]));
    assert!(!branches.contains("mixed"), "{branches}");
}

/// The pack of one object.
pub(crate) fn pack_of_one(kind: ObjectKind, data: &[u8]) -> (ObjectId, Vec<u8>) {
    let oid = ObjectId::hash_object(HashAlgo::Sha1, kind, data);
    let scratch = alt_os::temp::tempdir().unwrap();
    let mut writer = alt_git_pack::PackWriter::create(scratch.path(), HashAlgo::Sha1, 1).unwrap();
    writer.add(oid, kind, data).unwrap();
    let pack = std::fs::read(writer.finish().unwrap().pack_path).unwrap();
    (oid, pack)
}

/// What the push adds cannot be told through a tag that does not read
/// as one: the push is refused, with a reason, and nothing of it kept.
#[test]
fn a_new_value_at_a_tag_that_does_not_read_is_refused_and_not_kept() {
    let (origin, _) = seeded();
    require_signed_commits(origin.path());
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let (oid, pack) = pack_of_one(ObjectKind::Tag, b"not a tag");
    let name = oid.to_string();

    let report = push(&url, &[creation("refs/tags/with-pack", &name)], &pack);
    assert_eq!(report.unpack, Ok(()));
    assert!(refused_over(&report, "unreadable"), "{report:?}");
    let report = push(&url, &[creation("refs/tags/without-pack", &name)], &[]);
    assert!(
        refused_over(&report, "missing necessary objects"),
        "{report:?}"
    );
}

/// A push the server fails on: here the trust keys do not read when the
/// policy asks for them. None of its objects is in the odb afterwards.
#[test]
fn a_push_the_server_fails_on_leaves_none_of_its_objects() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    require_signed_commits(origin.path());
    std::fs::write(alt_dir.join("trust"), "no directory").unwrap();
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let (oid, commit) = commit_object(&seed, 1_700_000_000, "", "unsigned");
    let name = oid.to_string();

    let mut body = Vec::new();
    alt_wire::encode_push_request(
        &mut body,
        &[creation("refs/heads/with-pack", &name)],
        &["report-status"],
        HashAlgo::Sha1,
        &pack_of(&[(oid, commit)]),
    )
    .unwrap();
    let reply = alt_wire_http::GitTransport::new(url.as_str())
        .command(alt_wire_http::Service::ReceivePack, &body);
    assert!(reply.is_err(), "{reply:?}");
    // without the policy a ref may be created at any object the odb holds
    std::fs::remove_file(alt_dir.join("policy")).unwrap();
    std::fs::remove_file(alt_dir.join("trust")).unwrap();
    let report = push(&url, &[creation("refs/heads/without-pack", &name)], &[]);
    assert!(
        refused_over(&report, "missing necessary objects"),
        "{report:?}"
    );
}
