//! `alt push` of a ref whose objects the server already holds, against a
//! real `git receive-pack`.
//!
//! Git wants a pack behind every push that creates or updates a ref, an
//! empty one when there is nothing to send, and no pack at all behind a
//! push that only deletes.

use std::path::Path;

use crate::push::{alt, empty_server_repo, git, ok};
use crate::wire_test_server;

struct Pushed {
    server: alt_os::temp::TempDir,
    local: alt_os::temp::TempDir,
    url: String,
    /// the commit `older` points at, an ancestor of `main`
    first: String,
}

fn tip(root: &Path, rev: &str) -> String {
    let log = ok(
        "alt log",
        alt(root, &["log", "--pretty=oneline", "-n", "1", rev]),
    );
    log.split_whitespace().next().unwrap().to_owned()
}

/// A local repository with `main` two commits long and `older` left at the
/// first one, and a server that has received `main` only.
fn pushed_main() -> Pushed {
    let server = empty_server_repo();
    let url = wire_test_server::spawn(server.path().to_owned());
    let local = alt_os::temp::tempdir().unwrap();
    let root = local.path();
    ok("alt init", alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    ok("alt add", alt(root, &["add", "."]));
    ok("alt commit", alt(root, &["commit", "-m", "first"]));
    ok("alt branch", alt(root, &["branch", "older"]));
    std::fs::write(root.join("a.txt"), "hello v2\n").unwrap();
    ok("alt add", alt(root, &["add", "a.txt"]));
    ok("alt commit", alt(root, &["commit", "-m", "second"]));
    ok(
        "alt remote add",
        alt(root, &["remote", "add", "origin", &url]),
    );
    ok("alt push main", alt(root, &["push", "origin", "main"]));
    let first = tip(root, "refs/heads/older");
    assert_ne!(first, tip(root, "refs/heads/main"));
    Pushed {
        server,
        local,
        url,
        first,
    }
}

fn server_ref(server: &Path, name: &str) -> String {
    git(server, &["rev-parse", "--verify", name])
        .trim()
        .to_owned()
}

#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn a_new_branch_at_a_commit_the_server_has_is_created() {
    let p = pushed_main();
    let stdout = ok(
        "alt push older",
        alt(p.local.path(), &["push", "origin", "older", "--json"]),
    );
    assert!(stdout.contains("\"objects\":0"), "{stdout}");
    assert!(
        stdout.contains(r#""name":"refs/heads/older","status":"ok""#),
        "{stdout}"
    );
    assert_eq!(server_ref(p.server.path(), "refs/heads/older"), p.first);
    git(p.server.path(), &["fsck", "--strict", "--no-dangling"]);
}

#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn a_new_tag_at_a_commit_the_server_has_is_created() {
    let p = pushed_main();
    let root = p.local.path();
    ok("alt tag", alt(root, &["tag", "v1", &p.first]));
    let stdout = ok(
        "alt push tag",
        alt(root, &["push", "origin", "refs/tags/v1", "--json"]),
    );
    assert!(stdout.contains("\"objects\":0"), "{stdout}");
    assert_eq!(server_ref(p.server.path(), "refs/tags/v1"), p.first);
}

#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn one_ref_with_new_objects_and_one_without_go_in_the_same_push() {
    let p = pushed_main();
    let root = p.local.path();
    std::fs::write(root.join("c.txt"), "new\n").unwrap();
    ok("alt add", alt(root, &["add", "c.txt"]));
    ok("alt commit", alt(root, &["commit", "-m", "third"]));
    let stdout = ok(
        "alt push main older",
        alt(root, &["push", "origin", "main", "older", "--json"]),
    );
    assert!(stdout.contains("\"objects\":3"), "{stdout}");
    assert!(!stdout.contains("\"status\":\"ng\""), "{stdout}");
    assert_eq!(
        server_ref(p.server.path(), "refs/heads/main"),
        tip(root, "refs/heads/main")
    );
    assert_eq!(server_ref(p.server.path(), "refs/heads/older"), p.first);
    git(p.server.path(), &["fsck", "--strict", "--no-dangling"]);
}

/// The request a push that only deletes sends: its command and nothing
/// after the flush.
#[test]
#[ignore = "requires system git; run with --include-ignored locally"]
fn a_push_that_only_deletes_carries_no_pack() {
    let p = pushed_main();
    ok(
        "alt push older",
        alt(p.local.path(), &["push", "origin", "older"]),
    );
    let delete = alt_wire::RefUpdate {
        old: Some(p.first.parse().unwrap()),
        new: None,
        name: "refs/heads/older".into(),
    };
    let mut body = Vec::new();
    alt_wire::encode_push_request(
        &mut body,
        &[delete],
        &["report-status"],
        alt_git_codec::HashAlgo::Sha1,
        &[],
    )
    .unwrap();
    assert!(body.ends_with(b"0000"));
    let reply = alt_wire_http::GitTransport::new(p.url.as_str())
        .command(alt_wire_http::Service::ReceivePack, &body)
        .unwrap();
    let report = alt_wire::parse_report_status(&mut reply.as_slice()).unwrap();
    assert_eq!(report.unpack, Ok(()));
    assert_eq!(
        report.commands,
        vec![alt_wire::CommandStatus::Ok("refs/heads/older".into())]
    );
    let refs = git(p.server.path(), &["for-each-ref", "--format=%(refname)"]);
    assert_eq!(refs.trim(), "refs/heads/main");
}
