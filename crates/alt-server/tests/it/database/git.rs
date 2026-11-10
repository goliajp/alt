use super::*;
use base64::Engine;
use std::{
    path::Path,
    process::{Command, Output},
};

fn git(directory: &Path, token: &str, args: &[&str]) -> Output {
    let auth = base64::engine::general_purpose::STANDARD.encode(format!("alt:{token}"));
    Command::new("git")
        .args(args)
        .current_dir(directory)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "http.extraHeader")
        .env("GIT_CONFIG_VALUE_0", format!("Authorization: Basic {auth}"))
        .output()
        .unwrap()
}
pub(super) fn ok(directory: &Path, token: &str, args: &[&str]) -> Vec<u8> {
    let output = git(directory, token, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
pub(super) fn identity(directory: &Path, token: &str) {
    ok(directory, token, &["config", "user.name", "Original Actor"]);
    ok(
        directory,
        token,
        &["config", "user.email", "original@example.test"],
    );
}
pub(super) fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap().trim().to_owned()
}

fn packed_blob_size(directory: &Path, token: &str, oid: &str) -> usize {
    let index = std::fs::read_dir(directory.join(".git/objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "idx"))
        .expect("clone must preserve the received pack");
    let listing = text(ok(
        directory,
        token,
        &["verify-pack", "-v", index.to_str().unwrap()],
    ));
    let columns: Vec<_> = listing
        .lines()
        .find(|line| line.starts_with(oid))
        .unwrap()
        .split_whitespace()
        .collect();
    assert_eq!(columns[1], "blob");
    assert_eq!(columns.len(), 5, "fixture blob must remain a plain entry");
    columns[3].parse().unwrap()
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn real_git_cached_representation_requires_current_original_authority_and_survives_cache_loss() {
    let mut f = Fixture::new();
    let server = Running::start(&f);
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    ok(&source, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(&source, &f.token);
    // Both loose objects and the incoming plain pack use a deliberately
    // non-default encoding, making actual reuse observable without timing.
    for key in ["core.compression", "pack.compression", "pack.window"] {
        ok(&source, &f.token, &["config", key, "0"]);
    }
    let original = vec![b'x'; 64 * 1024];
    std::fs::write(source.join("payload"), &original).unwrap();
    ok(&source, &f.token, &["add", "."]);
    ok(&source, &f.token, &["commit", "-qm", "original encoding"]);
    let head = text(ok(&source, &f.token, &["rev-parse", "HEAD"]));
    let blob = text(ok(&source, &f.token, &["rev-parse", "HEAD:payload"]));
    let url = format!("{}/git/repository", server.url);
    ok(&source, &f.token, &["push", &url, "main"]);
    // Work-byte observations must cover the actual new physical originals.
    let metrics = server
        .send("GET", "/v1/instance/metrics", Some(&f.token), b"")
        .into_string()
        .unwrap();
    let persisted: Vec<_> = metrics
        .lines()
        .filter(|line| {
            line.starts_with("alt_work_bytes_total{") && line.contains("phase=\"persist\"")
        })
        .collect();
    assert_eq!(
        persisted.len(),
        1,
        "missing or duplicated persistence metric: {metrics}"
    );
    let observed: u64 = persisted[0].rsplit_once(' ').unwrap().1.parse().unwrap();
    let expected: i64 = f.client().query_one("SELECT sum(bytes)::bigint FROM alt_blobs WHERE repository='repository' AND state='stored'", &[]).unwrap().get(0);
    assert!(expected > 0);
    assert_eq!(observed, expected as u64);

    for service in ["git-upload-pack", "git-receive-pack"] {
        let response = ureq::get(&format!("{url}/info/refs?service={service}"))
            .set("Authorization", &format!("Bearer {}", f.token))
            .set("Git-Protocol", "version=2")
            .call()
            .unwrap();
        assert_eq!(response.status(), 200);
        let body = response.into_string().unwrap();
        if service == "git-upload-pack" {
            assert!(body.contains("version 2\n") && body.contains("fetch=wait-for-done\n"));
        } else {
            assert!(body.contains(&head) && body.contains("refs/heads/main"));
        }
    }
    let clone = |url: &str, name: &str| {
        ok(
            temp.path(),
            &f.token,
            &["-c", "fetch.unpackLimit=0", "clone", "--quiet", url, name],
        );
        let destination = temp.path().join(name);
        assert_eq!(
            std::fs::read(destination.join("payload")).unwrap(),
            original
        );
        assert_eq!(
            text(ok(&destination, &f.token, &["rev-parse", "HEAD"])),
            head
        );
        ok(&destination, &f.token, &["fsck", "--strict", "--full"]);
        packed_blob_size(&destination, &f.token, &blob)
    };
    assert!(clone(&url, "warm") > original.len());

    // A warm encoding cannot supply missing or damaged authoritative bytes.
    let digest = blake3::hash(&original).to_hex().to_string();
    let row = f.client().query_one("SELECT segment_id,byte_offset FROM alt_blob_locations WHERE repository='repository' AND blake3=$1", &[&digest]).unwrap();
    let segment: String = row.get(0);
    let offset = row.get::<_, i64>(1) as usize;
    let path = f
        .root
        .path()
        .join("bytes/installation/repository")
        .join(format!("segment-{segment}"));
    let container = std::fs::read(&path).unwrap();
    assert_eq!(&container[offset..offset + original.len()], &original);
    // The HTTP response must stop at this member, even when other originals
    // and the segment trailer follow it in the same physical file.
    let response = ureq::get(&format!(
        "{}/v1/repositories/repository/epochs/1/content/{digest}",
        server.url
    ))
    .set("Authorization", &format!("Bearer {}", f.token))
    .call()
    .unwrap();
    assert_eq!(
        response.header("Content-Length"),
        Some(original.len().to_string().as_str())
    );
    assert_eq!(response.header("Alt-Content-Blake3"), Some(digest.as_str()));
    let mut received = Vec::new();
    std::io::Read::read_to_end(&mut response.into_reader(), &mut received).unwrap();
    assert_eq!(received, original);
    let mut damaged = container.clone();
    damaged[offset..offset + original.len()].fill(b'y');
    std::fs::write(&path, damaged).unwrap();
    assert!(
        !git(
            temp.path(),
            &f.token,
            &["clone", "--quiet", &url, "corrupt"]
        )
        .status
        .success()
    );
    std::fs::remove_file(&path).unwrap();
    assert!(
        !git(
            temp.path(),
            &f.token,
            &["clone", "--quiet", &url, "missing"]
        )
        .status
        .success()
    );
    std::fs::write(&path, &container).unwrap();
    assert!(clone(&url, "restored") > original.len());
    f.store
        .set_grants(&f.credential, "repository", "owner", false, false, &[])
        .unwrap();
    assert!(
        !git(
            temp.path(),
            &f.token,
            &["clone", "--quiet", &url, "revoked"]
        )
        .status
        .success()
    );
    f.store
        .set_grants(&f.credential, "repository", "owner", true, true, &grants())
        .unwrap();
    assert!(clone(&url, "regranted") > original.len());

    // Restart loses the derived cache; canonical refs and bytes still clone.
    drop(server);
    let cold = Running::start(&f);
    assert!(clone(&format!("{}/git/repository", cold.url), "cold") < 1000);
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn real_git_atomic_push_clone_incremental_fetch_notes_tags_and_default_branch() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first");
    std::fs::create_dir(&first).unwrap();
    let url = format!("{}/git/repository", server.url);
    ok(&first, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(&first, &f.token);
    let source = (0..2000)
        .map(|i| format!("source line {i:04}: original independent bytes\n"))
        .collect::<String>();
    std::fs::write(first.join("source.txt"), &source).unwrap();
    let binary = [0u8, 255, 1, 0, 13, 10, 200];
    std::fs::write(first.join("asset.bin"), binary).unwrap();
    std::os::unix::fs::symlink("source.txt", first.join("source-link")).unwrap();
    ok(&first, &f.token, &["add", "."]);
    ok(&first, &f.token, &["commit", "-qm", "original source"]);
    let initial = text(ok(&first, &f.token, &["rev-parse", "HEAD"]));
    ok(
        &first,
        &f.token,
        &["tag", "-a", "v1", "-m", "release source"],
    );
    ok(
        &first,
        &f.token,
        &["tag", "-a", "v2", "v1", "-m", "nested original tag"],
    );
    ok(
        &first,
        &f.token,
        &["notes", "add", "-m", "original delivery facts", "HEAD"],
    );
    ok(&first, &f.token, &["branch", "aaa"]);
    ok(&first, &f.token, &["remote", "add", "origin", &url]);
    ok(
        &first,
        &f.token,
        &[
            "push",
            "--atomic",
            "origin",
            "main",
            "aaa",
            "refs/tags/v1",
            "refs/tags/v2",
            "refs/notes/commits",
        ],
    );
    assert_eq!(f.counts(), (5, 1, 1));
    ok(temp.path(), &f.token, &["clone", "--quiet", &url, "second"]);
    let second = temp.path().join("second");
    assert_eq!(
        text(ok(&second, &f.token, &["symbolic-ref", "HEAD"])),
        "refs/heads/main"
    );
    assert_eq!(text(ok(&second, &f.token, &["rev-parse", "HEAD"])), initial);
    assert_eq!(std::fs::read(second.join("asset.bin")).unwrap(), binary);
    assert_eq!(
        std::fs::read_link(second.join("source-link")).unwrap(),
        Path::new("source.txt")
    );
    assert_eq!(
        text(ok(&second, &f.token, &["rev-parse", "v1^{}"])),
        initial
    );
    assert_eq!(
        text(ok(&second, &f.token, &["rev-parse", "v2^{}"])),
        initial
    );
    let listed = text(ok(&first, &f.token, &["ls-remote", "--symref", &url]));
    assert!(
        listed.contains(&format!("{initial}\trefs/tags/v2^{{}}")),
        "{listed}"
    );
    std::fs::write(
        first.join("source.txt"),
        format!("{source}incremental reviewed change\n"),
    )
    .unwrap();
    ok(&first, &f.token, &["add", "source.txt"]);
    ok(&first, &f.token, &["commit", "-qm", "incremental source"]);
    let next = text(ok(&first, &f.token, &["rev-parse", "HEAD"]));
    ok(&first, &f.token, &["push", "origin", "main"]);
    ok(&second, &f.token, &["fetch", "origin"]);
    ok(&second, &f.token, &["merge", "--ff-only", "origin/main"]);
    assert_eq!(text(ok(&second, &f.token, &["rev-parse", "HEAD"])), next);
    assert_eq!(
        std::fs::read(first.join("source.txt")).unwrap(),
        std::fs::read(second.join("source.txt")).unwrap()
    );
    ok(
        &second,
        &f.token,
        &["fetch", "origin", "refs/notes/commits:refs/notes/commits"],
    );
    assert_eq!(
        ok(&first, &f.token, &["notes", "show", &initial]),
        ok(&second, &f.token, &["notes", "show", &initial])
    );
    ok(&second, &f.token, &["fsck", "--strict", "--full"]);
    assert_eq!(f.counts(), (5, 2, 2));
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn real_git_protected_branch_rejects_entire_atomic_push_and_force_without_review() {
    let mut f = Fixture::new();
    let server = Running::start(&f);
    let temp = tempfile::tempdir().unwrap();
    ok(
        temp.path(),
        &f.token,
        &["init", "-q", "--initial-branch=main"],
    );
    identity(temp.path(), &f.token);
    std::fs::write(temp.path().join("file"), "base\n").unwrap();
    ok(temp.path(), &f.token, &["add", "file"]);
    ok(temp.path(), &f.token, &["commit", "-qm", "base"]);
    let base = text(ok(temp.path(), &f.token, &["rev-parse", "HEAD"]));
    let url = format!("{}/git/repository", server.url);
    ok(temp.path(), &f.token, &["remote", "add", "origin", &url]);
    ok(temp.path(), &f.token, &["push", "origin", "main"]);
    f.store
        .set_policies(
            &f.credential,
            "repository",
            &[alt_repository_service::RefPolicy {
                refs: RefSelector::Exact("refs/heads/main".into()),
                allow_force: false,
                allow_delete: false,
                require_review: true,
            }],
        )
        .unwrap();
    std::fs::write(temp.path().join("file"), "new source\n").unwrap();
    ok(temp.path(), &f.token, &["add", "file"]);
    ok(temp.path(), &f.token, &["commit", "-qm", "new"]);
    let rejected = git(
        temp.path(),
        &f.token,
        &[
            "push",
            "--atomic",
            "origin",
            "main",
            "HEAD:refs/heads/topic",
        ],
    );
    assert!(!rejected.status.success());
    let info = f
        .store
        .repository_info(&f.credential, "repository")
        .unwrap();
    assert_eq!(info.refs["refs/heads/main"], base);
    assert!(!info.refs.contains_key("refs/heads/topic"));
    assert_eq!(f.counts(), (1, 1, 1));
    f.store
        .set_policies(
            &f.credential,
            "repository",
            &[alt_repository_service::RefPolicy {
                refs: RefSelector::Exact("refs/heads/main".into()),
                allow_force: false,
                allow_delete: false,
                require_review: false,
            }],
        )
        .unwrap();
    ok(temp.path(), &f.token, &["push", "origin", "main"]);
    let next = text(ok(temp.path(), &f.token, &["rev-parse", "HEAD"]));
    let rejected = git(
        temp.path(),
        &f.token,
        &[
            "push",
            "--force",
            "origin",
            &format!("{base}:refs/heads/main"),
        ],
    );
    assert!(!rejected.status.success());
    assert_eq!(
        f.store
            .repository_info(&f.credential, "repository")
            .unwrap()
            .refs["refs/heads/main"],
        next
    );
    assert_eq!(f.counts(), (1, 2, 2));
    let rejected = git(
        temp.path(),
        &f.token,
        &["push", "origin", ":refs/heads/main"],
    );
    assert!(!rejected.status.success());
    assert_eq!(f.counts(), (1, 2, 2));
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn real_git_many_refs_clone_and_bounded_gzip_requests() {
    use std::io::{Read, Write};
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = tempfile::tempdir().unwrap();
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
    let gzip = |bytes: &[u8]| {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    };
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

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; checks refreshed authority before returning a buffered pack"]
fn buffered_git_fetch_rechecks_refreshed_authority_before_output() {
    let mut f = Fixture::new();
    let server = Running::start(&f);
    let temp = tempfile::tempdir().unwrap();
    ok(
        temp.path(),
        &f.token,
        &["init", "-q", "--initial-branch=main"],
    );
    identity(temp.path(), &f.token);
    std::fs::write(
        temp.path().join("file"),
        b"buffered output remains private\n",
    )
    .unwrap();
    ok(temp.path(), &f.token, &["add", "file"]);
    ok(temp.path(), &f.token, &["commit", "-qm", "source"]);
    let head = text(ok(temp.path(), &f.token, &["rev-parse", "HEAD"]));
    ok(
        temp.path(),
        &f.token,
        &[
            "push",
            "--mirror",
            &format!("{}/git/repository", server.url),
        ],
    );
    let mut command = Vec::new();
    alt_wire::pkt::write_data(&mut command, b"command=fetch\n").unwrap();
    alt_wire::pkt::write_delim(&mut command).unwrap();
    alt_wire::pkt::write_data(&mut command, format!("want {head}\n").as_bytes()).unwrap();
    alt_wire::pkt::write_data(&mut command, b"done\n").unwrap();
    alt_wire::pkt::write_flush(&mut command).unwrap();
    let credential = f.credential.clone();
    let mut client = f.client();
    client.batch_execute("SET statement_timeout='2s'").unwrap();
    for route in [
        alt_server::Route::GitUpload("repository".into()),
        alt_server::Route::GitAdvertise("repository".into(), false),
        alt_server::Route::GitAdvertise("repository".into(), true),
    ] {
        let advertisement = matches!(&route, alt_server::Route::GitAdvertise(..));
        for fault in ["refresh", "grant", "valid"] {
            let mut refreshed = false;
            let before: i64 = client
                .query_one(
                    "SELECT count(*) FROM alt_pins WHERE pin_id LIKE 'read:%'",
                    &[],
                )
                .unwrap()
                .get(0);
            let result = alt_server::execute_git_with_refresh(
                &mut f.store,
                &credential,
                route.clone(),
                true,
                &mut std::io::Cursor::new(if advertisement {
                    Vec::new()
                } else {
                    command.clone()
                }),
                &mut |_| {
                    refreshed = true;
                    let during: i64 = client
                        .query_one(
                            "SELECT count(*) FROM alt_pins WHERE pin_id LIKE 'read:%'",
                            &[],
                        )
                        .unwrap()
                        .get(0);
                    if advertisement {
                        assert_eq!(during, before);
                    } else {
                        assert!(during > before);
                    }
                    if fault == "refresh" {
                        return Err(alt_server_store::Error::Unauthorized);
                    }
                    if fault == "grant" {
                        client
                            .execute("UPDATE alt_grants SET can_read=false", &[])
                            .unwrap();
                    }
                    Ok(credential.clone())
                },
            );
            assert!(refreshed, "did not finish local pack work: {fault}");
            if fault == "valid" {
                if advertisement {
                    assert!(matches!(result, Ok(alt_server::Output::Bytes { .. })));
                } else {
                    assert!(matches!(result, Ok(alt_server::Output::Temporary { .. })));
                }
            } else {
                assert!(matches!(result, Err(alt_server_store::Error::Unauthorized)));
            }
            client
                .execute("UPDATE alt_grants SET can_read=true", &[])
                .unwrap();
        }
    }
    assert_eq!(f.counts(), (1, 1, 1));
}
