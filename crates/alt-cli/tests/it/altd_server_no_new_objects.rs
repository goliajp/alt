//! `altd-server` receiving a push that creates a ref at a commit it already
//! holds: with the empty pack Git and alt send, with no pack at all (what
//! alt sent before it followed Git here), and never for an object it lacks.

use std::path::Path;
use std::process::Command;

use crate::altd_server::{alt, ok, spawn_server};

/// An alt repository with one commit on `main`; returns the commit too.
pub(crate) fn seeded() -> (alt_os::temp::TempDir, String) {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("seed.txt"), "seed\n").unwrap();
    ok(alt(root, &["add", "seed.txt"]));
    ok(alt(root, &["commit", "-m", "seed"]));
    let seed = tip(root, "refs/heads/main");
    (dir, seed)
}

pub(crate) fn tip(root: &Path, rev: &str) -> String {
    ok(alt(root, &["log", "--pretty=oneline", "-n", "1", rev]))
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

pub(crate) fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn alt_creates_a_branch_at_a_commit_the_server_has() {
    let (origin, seed) = seeded();
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let work = alt_os::temp::tempdir().unwrap();
    let clone = work.path().join("clone");
    ok(alt(
        work.path(),
        &["clone", url.as_str(), clone.to_str().unwrap()],
    ));
    ok(alt(&clone, &["branch", "again"]));
    let stdout = ok(alt(&clone, &["push", "origin", "again", "--json"]));
    assert!(stdout.contains("\"objects\":0"), "{stdout}");
    assert!(
        stdout.contains(r#""name":"refs/heads/again","status":"ok""#),
        "{stdout}"
    );
    drop(server);
    assert_eq!(tip(origin.path(), "refs/heads/again"), seed);
}

#[test]
fn git_creates_a_branch_and_a_tag_at_a_commit_the_server_has() {
    let (origin, seed) = seeded();
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let work = alt_os::temp::tempdir().unwrap();
    git(work.path(), &["clone", "-q", &url, "clone"]);
    git(
        &work.path().join("clone"),
        &[
            "push",
            "-q",
            "origin",
            "HEAD:refs/heads/again",
            "HEAD:refs/tags/v1",
        ],
    );
    drop(server);
    assert_eq!(tip(origin.path(), "refs/heads/again"), seed);
    assert_eq!(tip(origin.path(), "refs/tags/v1"), seed);
}

/// The command that points `name`, absent so far, at `new`.
pub(crate) fn creation(name: &str, new: &str) -> alt_wire::RefUpdate {
    alt_wire::RefUpdate {
        old: None,
        new: Some(new.parse().unwrap()),
        name: name.into(),
    }
}

/// A push of `updates`, with `pack` behind the flush.
pub(crate) fn push(
    url: &str,
    updates: &[alt_wire::RefUpdate],
    pack: &[u8],
) -> alt_wire::ReportStatus {
    let mut body = Vec::new();
    alt_wire::encode_push_request(
        &mut body,
        updates,
        &["report-status"],
        alt_git_codec::HashAlgo::Sha1,
        pack,
    )
    .unwrap();
    let reply = alt_wire_http::GitTransport::new(url)
        .command(alt_wire_http::Service::ReceivePack, &body)
        .unwrap();
    alt_wire::parse_report_status(&mut reply.as_slice()).unwrap()
}

fn create(url: &str, name: &str, new: &str, pack: &[u8]) -> alt_wire::ReportStatus {
    push(url, &[creation(name, new)], pack)
}

/// A pack of no objects.
fn empty_pack() -> Vec<u8> {
    let scratch = alt_os::temp::tempdir().unwrap();
    let written =
        alt_git_pack::PackWriter::create(scratch.path(), alt_git_codec::HashAlgo::Sha1, 0)
            .unwrap()
            .finish()
            .unwrap();
    std::fs::read(&written.pack_path).unwrap()
}

#[test]
fn a_create_needs_its_object_with_an_empty_pack_or_none() {
    let (origin, seed) = seeded();
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let empty = empty_pack();
    let accepted = |name: &str| vec![alt_wire::CommandStatus::Ok(name.to_owned())];

    let report = create(&url, "refs/heads/with-pack", &seed, &empty);
    assert_eq!(report.unpack, Ok(()));
    assert_eq!(report.commands, accepted("refs/heads/with-pack"));
    // an alt older than the empty pack sends nothing behind its commands
    let report = create(&url, "refs/heads/without-pack", &seed, &[]);
    assert_eq!(report.unpack, Ok(()));
    assert_eq!(report.commands, accepted("refs/heads/without-pack"));

    let absent = "1".repeat(40);
    for (name, pack) in [
        ("refs/heads/absent-with-pack", &empty[..]),
        ("refs/heads/absent-without-pack", &[][..]),
    ] {
        let report = create(&url, name, &absent, pack);
        assert!(
            matches!(
                report.commands.as_slice(),
                [alt_wire::CommandStatus::Ng { .. }]
            ),
            "{name}: {report:?}"
        );
    }
    drop(server);
    assert_eq!(tip(origin.path(), "refs/heads/with-pack"), seed);
    assert_eq!(tip(origin.path(), "refs/heads/without-pack"), seed);
    let branches = ok(alt(origin.path(), &["branch"]));
    assert!(!branches.contains("absent"), "{branches}");
}

/// A creation at an object the server lacks refuses the commands sent with
/// it: neither the creation nor the deletion beside it takes effect.
#[test]
fn an_absent_object_refuses_the_creations_and_deletions_sent_with_it() {
    let (origin, seed) = seeded();
    ok(alt(origin.path(), &["branch", "kept"]));
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let empty = empty_pack();
    let absent = "1".repeat(40);
    let delete = alt_wire::RefUpdate {
        old: Some(seed.parse().unwrap()),
        new: None,
        name: "refs/heads/kept".into(),
    };
    for (n, pack) in [&empty[..], &[][..]].into_iter().enumerate() {
        let updates = [
            creation(&format!("refs/heads/present-{n}"), &seed),
            delete.clone(),
            creation(&format!("refs/heads/absent-{n}"), &absent),
        ];
        let report = push(&url, &updates, pack);
        assert_eq!(report.commands.len(), 3, "{report:?}");
        for status in &report.commands {
            assert!(
                matches!(status, alt_wire::CommandStatus::Ng { .. }),
                "pack of {} bytes: {report:?}",
                pack.len()
            );
        }
    }
    drop(server);
    assert_eq!(tip(origin.path(), "refs/heads/kept"), seed);
    let branches = ok(alt(origin.path(), &["branch"]));
    assert!(!branches.contains("present"), "{branches}");
    assert!(!branches.contains("absent"), "{branches}");
}

/// The signature a push carries is over the commands it sends: with a ref
/// left out because the server has it, the server still verifies it.
#[test]
fn a_signed_push_with_a_ref_the_server_has_is_verified() {
    let (origin, seed) = seeded();
    std::fs::write(
        origin.path().join(".alt").join("policy"),
        "human:anonymous -> require-signed\n",
    )
    .unwrap();
    let server = spawn_server(origin.path());
    let url = format!("http://{}/", server.addr);
    let work = alt_os::temp::tempdir().unwrap();
    let clone = work.path().join("clone");
    ok(alt(
        work.path(),
        &["clone", url.as_str(), clone.to_str().unwrap()],
    ));
    crate::altd_server::enable_signed_push(&clone, origin.path(), "alice");
    ok(alt(&clone, &["branch", "again"]));
    let stdout = ok(alt(&clone, &["push", "origin", "main", "again", "--json"]));
    assert!(
        stdout.contains(concat!(
            r#""refs":[{"name":"refs/heads/again","status":"ok"},"#,
            r#"{"name":"refs/heads/main","status":"up-to-date"}],"#
        )),
        "{stdout}"
    );
    drop(server);
    assert_eq!(tip(origin.path(), "refs/heads/again"), seed);
}
