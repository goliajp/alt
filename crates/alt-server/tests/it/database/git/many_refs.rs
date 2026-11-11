use super::*;

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn real_git_many_refs_clone_and_bounded_gzip_requests() {
    use std::io::Read;
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let first = temp.path().join("first");
    std::fs::create_dir(&first).unwrap();
    ok(&first, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(&first, &f.token);
    std::fs::write(
        first.join("file"),
        b"many refs preserve the original bytes\n",
    )
    .unwrap();
    ok(&first, &f.token, &["add", "file"]);
    ok(&first, &f.token, &["commit", "-qm", "source"]);
    let head = text(ok(&first, &f.token, &["rev-parse", "HEAD"]));
    for index in 0..64 {
        ok(&first, &f.token, &["branch", &format!("topic-{index:03}")]);
    }
    let url = format!("{}/git/repository", server.url);
    ok(&first, &f.token, &["push", "--mirror", &url]);
    ok(temp.path(), &f.token, &["clone", "--quiet", &url, "second"]);
    let second = temp.path().join("second");
    assert_eq!(text(ok(&second, &f.token, &["rev-parse", "HEAD"])), head);
    assert_eq!(
        std::fs::read(second.join("file")).unwrap(),
        std::fs::read(first.join("file")).unwrap()
    );
    assert_eq!(
        text(ok(&second, &f.token, &["ls-remote", "--refs", &url]))
            .lines()
            .count(),
        65
    );
    ok(&second, &f.token, &["fsck", "--strict", "--full"]);

    let mut command = Vec::new();
    alt_wire::pkt::write_data(&mut command, b"command=ls-refs\n").unwrap();
    alt_wire::pkt::write_delim(&mut command).unwrap();
    alt_wire::pkt::write_data(&mut command, b"ref-prefix refs/heads/\n").unwrap();
    alt_wire::pkt::write_flush(&mut command).unwrap();
    let gzip = |bytes: &[u8]| alt_deflate::compress(bytes, 6, alt_deflate::Format::Gzip);
    let send = |path: &str, encoding: &str, body: &[u8], authorized: bool| {
        let mut request = ureq::post(&format!("{}{path}", server.url))
            .timeout(Duration::from_secs(15))
            .set("Git-Protocol", "version=2")
            .set("Content-Type", "application/x-git-upload-pack-request")
            .set("Content-Encoding", encoding);
        if authorized {
            request = request.set("Authorization", &format!("Bearer {}", f.token));
        }
        let response = match request.send_bytes(body) {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(error) => panic!("gzip HTTP request failed: {error}"),
        };
        let status = response.status();
        let mut bytes = Vec::new();
        response.into_reader().read_to_end(&mut bytes).unwrap();
        (status, bytes)
    };
    let path = "/git/repository/git-upload-pack";
    let compressed = gzip(&command);
    let (status, refs) = send(path, "gzip", &compressed, true);
    assert_eq!(status, 200);
    assert!(
        refs.windows(head.len())
            .any(|value| value == head.as_bytes())
    );
    let split = command.len() / 2;
    let mut members = gzip(&command[..split]);
    members.extend(gzip(&command[split..]));
    assert_eq!(send(path, "GZip", &members, true), (200, refs));
    let mut checksum = compressed.clone();
    let crc = checksum.len() - 8;
    checksum[crc] ^= 1;
    let mut trailing = compressed.clone();
    trailing.extend_from_slice(b"invalid trailing input");
    for invalid in [
        &compressed[..compressed.len() - 1],
        &checksum,
        &trailing,
        b"",
    ] {
        assert_eq!(send(path, "gzip", invalid, true).0, 400);
    }
    let oversized = gzip(&vec![b'x'; limits().request_bytes + 1]);
    assert!(oversized.len() < limits().request_bytes);
    assert_eq!(send(path, "gzip", &oversized, true).0, 413);
    assert_eq!(send(path, "gzip", &oversized, false).0, 401);
    assert_eq!(send(path, "gzip, gzip", &compressed, true).0, 415);
    assert_eq!(send(path, "br", &compressed, true).0, 415);
    assert_eq!(
        send(
            "/v1/repositories/repository/epochs/1/requests",
            "gzip",
            &compressed,
            true
        )
        .0,
        415
    );
    assert_eq!(f.counts(), (65, 1, 1));
}

/// one upload-pack request sent as it is; the status and the body that came back
fn upload(server: &Running, token: &str, body: &[u8]) -> (u16, Vec<u8>) {
    use std::io::Read;
    let response = match ureq::post(&format!("{}/git/repository/git-upload-pack", server.url))
        .timeout(Duration::from_secs(30))
        .set("Git-Protocol", "version=2")
        .set("Content-Type", "application/x-git-upload-pack-request")
        .set("Authorization", &format!("Bearer {token}"))
        .send_bytes(body)
    {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("upload-pack request failed: {error}"),
    };
    let status = response.status();
    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes).unwrap();
    (status, bytes)
}

fn command(name: &str, arguments: &[String]) -> Vec<u8> {
    let mut command = Vec::new();
    alt_wire::pkt::write_data(&mut command, format!("command={name}\n").as_bytes()).unwrap();
    alt_wire::pkt::write_delim(&mut command).unwrap();
    for argument in arguments {
        alt_wire::pkt::write_data(&mut command, format!("{argument}\n").as_bytes()).unwrap();
    }
    alt_wire::pkt::write_flush(&mut command).unwrap();
    command
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_repository_with_more_references_than_one_read_takes_answers_from_the_ones_a_request_names() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let first = temp.path().join("first");
    std::fs::create_dir(&first).unwrap();
    ok(&first, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(&first, &f.token);
    std::fs::write(first.join("file"), b"one\n").unwrap();
    ok(&first, &f.token, &["add", "file"]);
    ok(&first, &f.token, &["commit", "-qm", "one"]);
    let head = text(ok(&first, &f.token, &["rev-parse", "HEAD"]));
    let groups = alt_server_store::DEFERRED_REFS / 100 + 1;
    let mut packed = String::from("# pack-refs with: peeled fully-peeled sorted \n");
    for group in 0..groups {
        for index in 0..100 {
            packed.push_str(&format!("{head} refs/heads/g{group:02}/t{index:02}\n"));
        }
    }
    packed.push_str(&format!("{head} refs/heads/main\n"));
    std::fs::write(first.join(".git/packed-refs"), packed).unwrap();
    std::fs::remove_file(first.join(".git/refs/heads/main")).unwrap();
    let url = format!("{}/git/repository", server.url);
    ok(&first, &f.token, &["push", "-q", &url, "refs/heads/main"]);
    for group in 0..groups {
        let refspec = format!("refs/heads/g{group:02}/*:refs/heads/g{group:02}/*");
        ok(&first, &f.token, &["push", "-q", &url, &refspec]);
    }
    let total = groups * 100 + 1;
    assert!(total > alt_server_store::DEFERRED_REFS);

    let listed = text(ok(&first, &f.token, &["ls-remote", "--refs", &url]));
    assert_eq!(listed.lines().count(), total);
    assert!(listed.lines().all(|line| line.starts_with(&head)));
    assert_eq!(
        text(ok(
            &first,
            &f.token,
            &["ls-remote", &url, "refs/heads/g07/t42"]
        )),
        format!("{head}\trefs/heads/g07/t42")
    );
    let (status, lines) = upload(
        &server,
        &f.token,
        &command(
            "ls-refs",
            &[
                "symrefs".into(),
                "ref-prefix HEAD".into(),
                "ref-prefix refs/heads/g07/".into(),
            ],
        ),
    );
    assert_eq!(status, 200);
    let mut expected = Vec::new();
    alt_wire::pkt::write_data(
        &mut expected,
        format!("{head} HEAD symref-target:refs/heads/main\n").as_bytes(),
    )
    .unwrap();
    for index in 0..100 {
        alt_wire::pkt::write_data(
            &mut expected,
            format!("{head} refs/heads/g07/t{index:02}\n").as_bytes(),
        )
        .unwrap();
    }
    alt_wire::pkt::write_flush(&mut expected).unwrap();
    assert_eq!(lines, expected);

    // one branch: a clone of every branch names each of them, more lines than the
    // command limit of this fixture allows
    ok(
        temp.path(),
        &f.token,
        &[
            "clone",
            "--quiet",
            "--single-branch",
            "--branch",
            "main",
            &url,
            "second",
        ],
    );
    let second = temp.path().join("second");
    assert_eq!(text(ok(&second, &f.token, &["rev-parse", "HEAD"])), head);
    ok(&second, &f.token, &["fsck", "--strict", "--full"]);

    std::fs::write(first.join("file"), b"two\n").unwrap();
    ok(&first, &f.token, &["commit", "-qam", "two"]);
    let next = text(ok(&first, &f.token, &["rev-parse", "HEAD"]));
    ok(&first, &f.token, &["tag", "-am", "release", "v1"]);
    ok(
        &first,
        &f.token,
        &["push", "-q", &url, "HEAD:refs/heads/other", "refs/tags/v1"],
    );
    ok(
        &second,
        &f.token,
        &["fetch", "-q", &url, "refs/heads/other:refs/heads/other"],
    );
    assert_eq!(text(ok(&second, &f.token, &["rev-parse", "other"])), next);
    // the tag follows the commit it points at
    assert_eq!(
        text(ok(&second, &f.token, &["rev-parse", "v1^{commit}"])),
        next
    );
    ok(&second, &f.token, &["fsck", "--strict", "--full"]);

    // an object no reference points at cannot be asked for
    let (status, _) = upload(
        &server,
        &f.token,
        &command(
            "fetch",
            &[format!("want {}", "1".repeat(40)), "done".into()],
        ),
    );
    assert_ne!(status, 200);
    let (status, pack) = upload(
        &server,
        &f.token,
        &command(
            "fetch",
            &[
                format!("want {next}"),
                format!("have {head}"),
                "done".into(),
            ],
        ),
    );
    assert_eq!(status, 200);
    assert!(pack.windows(4).any(|window| window == b"PACK"));
}
