use super::*;
use std::{
    path::Path,
    process::{Command, Output},
};

fn git(directory: &Path, token: &str, args: &[&str]) -> Output {
    let auth = alt_base64::STANDARD.encode(format!("alt:{token}"));
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

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn real_git_atomic_push_clone_incremental_fetch_notes_tags_and_default_branch() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
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
    let temp = alt_os::temp::tempdir().unwrap();
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

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn real_git_default_client_pushes_a_pack_larger_than_its_post_buffer() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let first = temp.path().join("first");
    std::fs::create_dir(&first).unwrap();
    let url = format!("{}/git/repository", server.url);
    ok(&first, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(&first, &f.token);
    // incompressible, so the pack exceeds git's default 1 MiB post buffer and the
    // client probes with a lone flush packet before a chunked request
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let noise: Vec<u8> = (0..3 * 1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    std::fs::write(first.join("asset.bin"), &noise).unwrap();
    ok(&first, &f.token, &["add", "."]);
    ok(&first, &f.token, &["commit", "-qm", "large asset"]);
    ok(&first, &f.token, &["push", "-q", &url, "main"]);
    ok(temp.path(), &f.token, &["clone", "--quiet", &url, "second"]);
    let second = temp.path().join("second");
    assert_eq!(std::fs::read(second.join("asset.bin")).unwrap(), noise);
    ok(&second, &f.token, &["fsck", "--strict", "--full"]);
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn real_git_small_pushes_are_merged_and_stay_readable() {
    let mut f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let first = temp.path().join("first");
    std::fs::create_dir(&first).unwrap();
    let url = format!("{}/git/repository", server.url);
    ok(&first, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(&first, &f.token);
    let pushes = 40;
    for index in 0..pushes {
        let body: String = (0..300)
            .map(|line| {
                format!(
                    "line {line} of push {}\n",
                    if line % 29 == index % 29 { index } else { 0 }
                )
            })
            .collect();
        std::fs::write(first.join("source.txt"), body).unwrap();
        ok(&first, &f.token, &["add", "."]);
        ok(
            &first,
            &f.token,
            &["commit", "-qm", &format!("push {index}")],
        );
        ok(&first, &f.token, &["push", "-q", &url, "main"]);
    }
    let count = |f: &Fixture, state: &str| -> i64 {
        f.client()
            .query_one(
                "SELECT count(*) FROM alt_pack_segments WHERE state=$1",
                &[&state],
            )
            .unwrap()
            .get(0)
    };
    // merging runs behind the pushes; without it there would be one segment per push
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while count(&f, "live") > 12 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        count(&f, "live") <= 12,
        "live segments: {}",
        count(&f, "live")
    );
    assert!(count(&f, "retired") > 0);

    ok(temp.path(), &f.token, &["clone", "--quiet", &url, "second"]);
    let second = temp.path().join("second");
    ok(&second, &f.token, &["fsck", "--strict", "--full"]);
    assert_eq!(
        text(ok(&second, &f.token, &["rev-list", "--count", "HEAD"])),
        pushes.to_string()
    );
    std::fs::write(first.join("source.txt"), "after the merges\n").unwrap();
    ok(&first, &f.token, &["commit", "-qam", "one more"]);
    ok(&first, &f.token, &["push", "-q", &url, "main"]);
    ok(&second, &f.token, &["pull", "-q", "--ff-only"]);
    assert_eq!(
        std::fs::read_to_string(second.join("source.txt")).unwrap(),
        "after the merges\n"
    );

    // once their time is up the retired segments leave the disk; nothing misses them
    let retired = count(&f, "retired");
    let removed = f.store.sweep_packs("repository", 0).unwrap();
    assert!(
        removed as i64 >= retired,
        "{removed} files for {retired} segments"
    );
    assert_eq!(count(&f, "retired"), 0);
    ok(temp.path(), &f.token, &["clone", "--quiet", &url, "third"]);
    let third = temp.path().join("third");
    ok(&third, &f.token, &["fsck", "--strict", "--full"]);
    assert_eq!(
        text(ok(&third, &f.token, &["rev-parse", "HEAD"])),
        text(ok(&first, &f.token, &["rev-parse", "HEAD"]))
    );
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn real_git_push_over_a_limit_is_told_which_limit() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let temp = alt_os::temp::tempdir().unwrap();
    let first = temp.path().join("first");
    std::fs::create_dir(&first).unwrap();
    let url = format!("{}/git/repository", server.url);
    ok(&first, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(&first, &f.token);
    // more objects than the configured bound of one publication
    for index in 0..1100 {
        std::fs::write(
            first.join(format!("file-{index}.txt")),
            format!("{index}\n"),
        )
        .unwrap();
    }
    ok(&first, &f.token, &["add", "."]);
    ok(&first, &f.token, &["commit", "-qm", "too many objects"]);
    let pushed = git(&first, &f.token, &["push", &url, "main"]);
    assert!(!pushed.status.success());
    let stderr = String::from_utf8_lossy(&pushed.stderr);
    assert!(
        stderr.contains("server limit exceeded: objects"),
        "{stderr}"
    );
    assert_eq!(f.counts().0, 0);
}
