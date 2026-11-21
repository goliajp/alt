//! `altd-server` telling what a push adds to the history served, with
//! `require-signed-commits` in force: against the refs as they are when
//! the refs move, whatever stands under them, and with nothing written
//! for a push that is refused.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_os::sys::{LOCK_EX, flock};
use alt_refs::{RefChange, RefStore, RefTarget};
use alt_wire::CommandStatus;

use crate::altd_server::spawn_server;
use crate::altd_server_no_new_objects::{creation, push, seeded, tip};
use crate::altd_server_signed_commits::{
    accepted, commit_object, pack_of, pack_of_one, refused_over, require_signed_commits,
};

/// Has the served repository trust `alice`; her key.
fn trusted(origin: &Path) -> alt_sign::SecretKey {
    let (secret, public) = alt_sign::SecretKey::generate();
    let trust = origin.join(".alt").join("trust");
    std::fs::create_dir_all(&trust).unwrap();
    std::fs::write(trust.join("alice.pub"), public.to_text()).unwrap();
    secret
}

/// A commit on top of `parent` that `secret` signed as `alice`.
fn signed(secret: &alt_sign::SecretKey, parent: &str, message: &str) -> (ObjectId, Vec<u8>) {
    let (_, plain) = commit_object(parent, 1_700_000_000, "", message);
    let line = alt_cli::commit_sign::alt_sig_line("alice", &secret.sign(&plain));
    let bytes = alt_cli::commit_sign::embed_alt_sig(&plain, &line).unwrap();
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, &bytes),
        bytes,
    )
}

fn deletion(name: &str, old: &ObjectId) -> alt_wire::RefUpdate {
    alt_wire::RefUpdate {
        old: Some(*old),
        new: None,
        name: name.into(),
    }
}

fn all_accepted(report: &alt_wire::ReportStatus) -> bool {
    let ok = |status: &CommandStatus| matches!(status, CommandStatus::Ok(_));
    !report.commands.is_empty() && report.commands.iter().all(ok)
}

#[test]
fn refs_at_objects_that_do_not_read_do_not_stand_in_the_way_of_a_signed_push() {
    let (origin, seed) = seeded();
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let (tag, tag_pack) = pack_of_one(ObjectKind::Tag, b"not a tag");
    let (commit, commit_pack) = pack_of_one(ObjectKind::Commit, b"not a commit");
    let report = push(
        &url,
        &[creation("refs/tags/bad", &tag.to_string())],
        &tag_pack,
    );
    assert!(accepted(&report), "{report:?}");
    let report = push(
        &url,
        &[creation("refs/heads/bad", &commit.to_string())],
        &commit_pack,
    );
    assert!(accepted(&report), "{report:?}");

    // read anew by the server with the next request
    require_signed_commits(origin.path());
    let secret = trusted(origin.path());
    let good = signed(&secret, &seed, "signed");
    let name = good.0.to_string();
    let report = push(
        &url,
        &[creation("refs/heads/good", &name)],
        &pack_of(&[good]),
    );
    assert!(accepted(&report), "{report:?}");
    let report = push(
        &url,
        &[
            deletion("refs/tags/bad", &tag),
            deletion("refs/heads/bad", &commit),
        ],
        &[],
    );
    assert!(all_accepted(&report), "{report:?}");
    drop(server);
    assert_eq!(tip(origin.path(), "refs/heads/good"), name);
}

/// A client that lacks a commit the server advertises sends what lies
/// under it once more. Such a commit is in the pack and not added.
#[test]
fn an_unsigned_commit_of_the_pack_that_refs_reach_already_does_not_refuse_the_push() {
    let (origin, seed) = seeded();
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let served = commit_object(&seed, 1_700_000_000, "", "served");
    let report = push(
        &url,
        &[creation("refs/heads/base", &served.0.to_string())],
        &pack_of(std::slice::from_ref(&served)),
    );
    assert!(accepted(&report), "{report:?}");

    require_signed_commits(origin.path());
    let secret = trusted(origin.path());
    let next = signed(&secret, &served.0.to_string(), "next");
    let name = next.0.to_string();
    let report = push(
        &url,
        &[creation("refs/heads/next", &name)],
        &pack_of(&[served, next]),
    );
    assert!(accepted(&report), "{report:?}");

    // one that no new value reaches comes in with the pack, and is held
    // to the policy once a ref is to reach it
    let stray = commit_object(&seed, 1_700_000_000, "", "stray");
    let stray_name = stray.0.to_string();
    let last = signed(&secret, &name, "last");
    let last_name = last.0.to_string();
    let report = push(
        &url,
        &[creation("refs/heads/last", &last_name)],
        &pack_of(&[stray, last]),
    );
    assert!(accepted(&report), "{report:?}");
    let report = push(&url, &[creation("refs/heads/stray", &stray_name)], &[]);
    assert!(refused_over(&report, "missing alt-sig"), "{report:?}");
    drop(server);
    assert_eq!(tip(origin.path(), "refs/heads/next"), name);
    assert_eq!(tip(origin.path(), "refs/heads/last"), last_name);
}

/// The only ref that reaches an unsigned commit is deleted by another
/// process after the server has read the refs for a push that points a
/// ref at the commit. The server is held at `odb.lock`, which it takes
/// after reading the refs, for as long as that takes.
#[test]
fn a_commit_whose_last_ref_another_process_deleted_meanwhile_is_added() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let unsigned = commit_object(&seed, 1_700_000_000, "", "unsigned");
    let (oid, name) = (unsigned.0, unsigned.0.to_string());
    let report = push(
        &url,
        &[creation("refs/heads/only", &name)],
        &pack_of(&[unsigned]),
    );
    assert!(accepted(&report), "{report:?}");
    require_signed_commits(origin.path());

    let objects = File::open(alt_dir.join("odb.lock")).unwrap();
    flock(&objects, LOCK_EX).unwrap();
    let pushing =
        std::thread::spawn(move || push(&url, &[creation("refs/heads/again", &name)], &[]));
    // long enough for the server to get to the lock
    std::thread::sleep(Duration::from_millis(500));
    let gone = RefChange {
        name: "refs/heads/only".into(),
        old: Some(RefTarget::Oid(oid)),
        new: None,
    };
    let mut refs = RefStore::open(&alt_dir).unwrap();
    refs.commit("another process", 0, &[gone]).unwrap();
    drop(refs);
    drop(objects);
    let report = pushing.join().unwrap();
    assert!(refused_over(&report, "missing alt-sig"), "{report:?}");
}

/// The lengths of the odb's append files, and its marker: a rollback
/// moves the marker's generation.
fn odb_state(alt_dir: &Path) -> (Vec<Option<u64>>, Option<Vec<u8>>) {
    let files = [
        "store/pack-00000001.altpack",
        "store/manifests/blobmap",
        "tier1",
        "map.alt",
    ];
    let len = |name: &&str| std::fs::metadata(alt_dir.join(name)).ok().map(|m| m.len());
    (
        files.iter().map(len).collect(),
        std::fs::read(alt_dir.join("odb.durable")).ok(),
    )
}

/// No object of a refused push is written and taken out again: there is
/// no moment at which another process could find one in the odb.
#[test]
fn a_refused_push_writes_nothing_to_the_odb() {
    let (origin, seed) = seeded();
    let alt_dir = origin.path().join(".alt");
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let first = commit_object(&seed, 1_700_000_000, "", "first");
    let report = push(
        &url,
        &[creation("refs/heads/first", &first.0.to_string())],
        &pack_of(&[first]),
    );
    assert!(accepted(&report), "{report:?}");
    let before = odb_state(&alt_dir);
    assert!(before.0.iter().flatten().count() >= 3, "{before:?}");

    let refused = commit_object(&seed, 1_700_000_000, "", "refused");
    let name = refused.0.to_string();
    let pack = pack_of(&[refused]);
    // a new value the server lacks
    let absent = creation("refs/heads/absent", &"1".repeat(40));
    let report = push(
        &url,
        &[creation("refs/heads/refused", &name), absent],
        &pack,
    );
    assert!(!all_accepted(&report), "{report:?}");
    assert_eq!(odb_state(&alt_dir), before);
    // a ref that is not where the push expects it
    let report = push(&url, &[creation("refs/heads/main", &name)], &pack);
    assert!(refused_over(&report, "ref tx"), "{report:?}");
    assert_eq!(odb_state(&alt_dir), before);
    // a commit that fails the policy
    require_signed_commits(origin.path());
    let report = push(&url, &[creation("refs/heads/refused", &name)], &pack);
    assert!(refused_over(&report, "missing alt-sig"), "{report:?}");
    assert_eq!(odb_state(&alt_dir), before);

    let report = push(&url, &[creation("refs/heads/refused", &name)], &[]);
    assert!(
        refused_over(&report, "missing necessary objects"),
        "{report:?}"
    );
}
