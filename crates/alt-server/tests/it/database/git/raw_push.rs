//! Receive-pack requests built by hand: what Git would not send, and what
//! it sends only in cases its own client makes hard to reach.

use super::*;

/// Posts `body` to the repository's receive-pack; the status and the report.
fn receive(url: &str, token: &str, body: &[u8]) -> (u16, String) {
    use std::io::Read;
    let response = match ureq::post(&format!("{url}/git-receive-pack"))
        .timeout(Duration::from_secs(15))
        .set("Content-Type", "application/x-git-receive-pack-request")
        .set("Authorization", &format!("Bearer {token}"))
        .send_bytes(body)
    {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("push request failed: {error}"),
    };
    let status = response.status();
    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes).unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn real_git_push_with_unknown_capabilities_and_unchanged_refs() {
    use std::io::Write;
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let first = temp.path().join("first");
    std::fs::create_dir(&first).unwrap();
    let url = format!("{}/git/repository", server.url);
    ok(&first, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(&first, &f.token);
    std::fs::write(first.join("file"), "one\n").unwrap();
    ok(&first, &f.token, &["add", "file"]);
    ok(&first, &f.token, &["commit", "-qm", "one"]);
    let one = text(ok(&first, &f.token, &["rev-parse", "HEAD"]));
    ok(&first, &f.token, &["branch", "keep"]);
    ok(&first, &f.token, &["push", "-q", &url, "main", "keep"]);
    std::fs::write(first.join("file"), "two\n").unwrap();
    ok(&first, &f.token, &["commit", "-qam", "two"]);
    let two = text(ok(&first, &f.token, &["rev-parse", "HEAD"]));
    let mut packing = Command::new("git")
        .args(["pack-objects", "--stdout", "-q", "--revs"])
        .current_dir(&first)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    packing
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{two}\n^{one}\n").as_bytes())
        .unwrap();
    let pack = packing.wait_with_output().unwrap().stdout;

    // what a client newer than this server may send: capabilities the server has
    // never heard of, and refs listed although they do not change
    let request = |kept: &str| {
        let mut body = Vec::new();
        alt_wire::pkt::write_data(
            &mut body,
            format!(
                "{one} {two} refs/heads/main\0report-status alt-capability-from-the-future=1 another-one\n"
            )
            .as_bytes(),
        )
        .unwrap();
        alt_wire::pkt::write_data(
            &mut body,
            format!("{kept} {kept} refs/heads/keep\n").as_bytes(),
        )
        .unwrap();
        alt_wire::pkt::write_flush(&mut body).unwrap();
        body.extend_from_slice(&pack);
        receive(&url, &f.token, &body)
    };
    let refs = || text(ok(&first, &f.token, &["ls-remote", "--refs", &url]));

    // the unchanged ref is still an expectation: a stale one refuses the whole push
    let (status, report) = request(&two);
    assert_eq!(status, 200);
    assert!(report.contains("ng refs/heads/main"), "{report}");
    assert!(refs().contains(&format!("{one}\trefs/heads/main")));

    let (status, report) = request(&one);
    assert_eq!(status, 200);
    assert!(report.contains("ok refs/heads/main"), "{report}");
    let listed = refs();
    assert!(listed.contains(&format!("{two}\trefs/heads/main")));
    assert!(listed.contains(&format!("{one}\trefs/heads/keep")));
    ok(temp.path(), &f.token, &["clone", "--quiet", &url, "second"]);
    ok(
        &temp.path().join("second"),
        &f.token,
        &["fsck", "--strict", "--full"],
    );
}

/// A repository with one commit pushed to `main`; returns the commit.
fn one_commit(directory: &Path, token: &str, url: &str) -> String {
    ok(directory, token, &["init", "-q", "--initial-branch=main"]);
    identity(directory, token);
    std::fs::write(directory.join("file"), "one\n").unwrap();
    ok(directory, token, &["add", "file"]);
    ok(directory, token, &["commit", "-qm", "one"]);
    ok(directory, token, &["push", "-q", url, "main"]);
    text(ok(directory, token, &["rev-parse", "HEAD"]))
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn real_git_creates_a_branch_and_a_tag_at_a_commit_the_server_has() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let url = format!("{}/git/repository", server.url);
    let one = one_commit(temp.path(), &f.token, &url);
    // nothing to send: Git puts a pack of no objects behind the two commands
    ok(
        temp.path(),
        &f.token,
        &[
            "push",
            "-q",
            &url,
            "main:refs/heads/again",
            "main:refs/tags/v1",
        ],
    );
    let listed = text(ok(temp.path(), &f.token, &["ls-remote", "--refs", &url]));
    assert!(
        listed.contains(&format!("{one}\trefs/heads/again")),
        "{listed}"
    );
    assert!(listed.contains(&format!("{one}\trefs/tags/v1")), "{listed}");
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_create_needs_its_object_with_an_empty_pack_or_none() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let url = format!("{}/git/repository", server.url);
    let one = one_commit(temp.path(), &f.token, &url);
    let scratch = alt_os::temp::tempdir().unwrap();
    let written = alt_git_pack::PackWriter::create(scratch.path(), HashAlgo::Sha1, 0)
        .unwrap()
        .finish()
        .unwrap();
    let empty = std::fs::read(&written.pack_path).unwrap();
    let create = |name: &str, new: &str, pack: &[u8]| {
        let mut body = Vec::new();
        let line = format!("{} {new} {name}\0report-status\n", "0".repeat(40));
        alt_wire::pkt::write_data(&mut body, line.as_bytes()).unwrap();
        alt_wire::pkt::write_flush(&mut body).unwrap();
        body.extend_from_slice(pack);
        receive(&url, &f.token, &body).1
    };

    let report = create("refs/heads/with-pack", &one, &empty);
    assert!(report.contains("ok refs/heads/with-pack"), "{report}");
    // an alt older than the empty pack sends nothing behind its commands
    let report = create("refs/heads/without-pack", &one, &[]);
    assert!(report.contains("ok refs/heads/without-pack"), "{report}");
    let absent = "1".repeat(40);
    let report = create("refs/heads/absent-with-pack", &absent, &empty);
    assert!(
        report.contains("ng refs/heads/absent-with-pack"),
        "{report}"
    );
    let report = create("refs/heads/absent-without-pack", &absent, &[]);
    assert!(
        report.contains("ng refs/heads/absent-without-pack"),
        "{report}"
    );

    let listed = text(ok(temp.path(), &f.token, &["ls-remote", "--refs", &url]));
    assert!(
        listed.contains(&format!("{one}\trefs/heads/with-pack")),
        "{listed}"
    );
    assert!(
        listed.contains(&format!("{one}\trefs/heads/without-pack")),
        "{listed}"
    );
    assert!(!listed.contains("absent"), "{listed}");
}
