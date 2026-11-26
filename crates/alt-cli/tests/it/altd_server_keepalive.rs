//! A fetch whose pack takes long to make hears from the server meanwhile:
//! the head of the answer and the keepalives of `upload-pack`, as `git
//! http-backend` and `git upload-pack` send them. So does a push whose pack
//! takes long to take in, as `git receive-pack` sends its keepalives.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::mpsc;
use std::time::Duration;

use crate::altd_server::{alt, ok, spawn_server_with_env};
use crate::altd_server_sending::{answer, asked};
use crate::altd_server_slow_peers::noise;
use crate::altd_server_untrusted::repository;

/// A repository of `files` files that have nothing in common, and its head.
fn of_files(origin: &Path, files: usize) -> String {
    repository(origin);
    let all = noise(files * 8192);
    for (n, file) in all.chunks(8192).enumerate() {
        std::fs::write(origin.join(format!("file-{n}")), file).unwrap();
    }
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "files"]));
    ok(alt(origin, &["rev-parse", "HEAD"])).trim().to_owned()
}

/// The head of an answer, and its body put together from its chunks.
fn chunked(answer: &[u8]) -> (String, Vec<u8>) {
    let ended = answer.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let head = String::from_utf8_lossy(&answer[..ended]).into_owned();
    assert!(
        head.contains("\r\nTransfer-Encoding: chunked\r\n"),
        "{head}"
    );
    assert!(!head.contains("Content-Length"), "{head}");
    let (mut at, mut body) = (ended, Vec::new());
    loop {
        let line = answer[at..].windows(2).position(|w| w == b"\r\n").unwrap();
        let size = std::str::from_utf8(&answer[at..at + line]).unwrap();
        let size = usize::from_str_radix(size, 16).unwrap();
        at += line + 2;
        body.extend_from_slice(&answer[at..at + size]);
        assert_eq!(&answer[at + size..at + size + 2], b"\r\n");
        at += size + 2;
        if size == 0 {
            assert_eq!(at, answer.len());
            return (head, body);
        }
    }
}

/// The pkt-lines of `body`, each as its payload; a flush as nothing.
fn lines(body: &[u8]) -> Vec<Option<&[u8]>> {
    let (mut at, mut out) = (0, Vec::new());
    while at < body.len() {
        let size = std::str::from_utf8(&body[at..at + 4]).unwrap();
        let size = usize::from_str_radix(size, 16).unwrap();
        out.push((size >= 4).then(|| &body[at + 4..at + size]));
        at += size.max(4);
    }
    out
}

/// The head and the start of the body go out before the pack is made,
/// and a keepalive behind them until it is; the pack comes whole behind
/// those. A server that sends nothing before its answer is made sends it
/// with its length, and no keepalive.
#[test]
fn the_head_of_a_fetch_goes_out_and_keepalives_follow_while_its_pack_is_made() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    let head = of_files(&origin, 400);
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_KEEPALIVE_MS", "1"),
    ]);
    let (told, body) = chunked(&answer(asked(&server.addr, &head)));
    assert!(told.starts_with("HTTP/1.1 200 OK\r\n"), "{told}");
    assert!(
        told.contains("application/x-git-upload-pack-result"),
        "{told}"
    );
    let lines = lines(&body);
    assert_eq!(lines[0], Some(&b"packfile\n"[..]));
    let kept = lines[1..].iter().take_while(|l| *l == &Some(&b"\x01"[..]));
    assert!(kept.count() > 0, "no keepalive before the pack");
    let pack: Vec<u8> = lines[1..]
        .iter()
        .flatten()
        .flat_map(|l| {
            assert_eq!(l[0], 1, "a line of another band");
            l[1..].to_vec()
        })
        .collect();
    assert!(pack.starts_with(b"PACK"));
    assert_eq!(lines.last(), Some(&None));
    assert_eq!(lines.iter().filter(|l| l.is_none()).count(), 1);
}

/// What takes longer than a fetch may once its head went out is told in
/// the side band of errors, as `upload-pack` tells what stopped it; what
/// takes longer before that is a 503 as before. Which of the two a time
/// falls in is as fast as the host is, so a few are tried: each answer
/// is one of the two, and the times are such that the later ones fall
/// where the pack is made.
#[test]
fn a_fetch_stopped_once_its_head_went_out_is_told_why_in_the_side_band() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    let head = of_files(&origin, 4000);
    let mut in_band = 0;
    for ms in ["50", "200", "800", "3200"] {
        let server = spawn_server_with_env(&[
            ("ALT_SERVER_REPO", origin.to_str().unwrap()),
            ("ALT_SERVER_KEEPALIVE_MS", "1"),
            ("ALT_SERVER_MAX_FETCH_MS", ms),
        ]);
        let got = answer(asked(&server.addr, &head));
        if got.starts_with(b"HTTP/1.1 503") {
            let late = String::from_utf8_lossy(&got);
            assert!(late.ends_with("the fetch takes longer than the server gives one"));
            continue;
        }
        let (_, body) = chunked(&got);
        let why = &b"\x03error: the fetch takes longer than the server gives one"[..];
        // a flush where the pack was made in time
        if let Some(last) = lines(&body).last().unwrap() {
            assert_eq!(*last, why, "{:?}", String::from_utf8_lossy(last));
            in_band += 1;
        }
    }
    assert!(in_band > 0, "no time fell where the pack is made");
}

/// `alt clone` and `git clone` both take a pack that keepalives come before.
#[test]
fn a_clone_takes_a_pack_that_keepalives_come_before() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    let head = of_files(&origin, 400);
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_KEEPALIVE_MS", "1"),
    ]);
    let url = format!("http://{}/", server.addr);
    ok(alt(tmp.path(), &["clone", &url, "by-alt"]));
    let by_alt = ok(alt(&tmp.path().join("by-alt"), &["rev-parse", "HEAD"]));
    assert_eq!(by_alt.trim(), head);
    assert_eq!(
        std::fs::read(tmp.path().join("by-alt/file-399")).unwrap(),
        std::fs::read(origin.join("file-399")).unwrap()
    );
    let by_git = Command::new("git")
        .args(["clone", "-q", &url])
        .arg(tmp.path().join("by-git"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PROTOCOL", "version=2")
        .output()
        .unwrap();
    assert!(by_git.status.success(), "{by_git:?}");
    assert_eq!(
        std::fs::read(tmp.path().join("by-git/file-399")).unwrap(),
        std::fs::read(origin.join("file-399")).unwrap()
    );
}

/// Two pushes wait for the memory a third is indexed in for longer than
/// their clients let a connection be silent: `alt`, thirty seconds, and
/// `git` told to give up after two seconds of less than a byte a second.
/// They hear keepalives meanwhile, and land once the memory is theirs.
#[test]
fn a_push_that_waits_longer_than_its_client_hears_nothing_for_lands() {
    use crate::altd_server_index_memory::{Released, commit};
    use crate::altd_server_pack_limits::{git, push, source};
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    repository(&root.join("held"));
    repository(&root.join("other"));
    // a push to `held` stands, with its memory, until a key it is checked
    // against is read: the key is a pipe
    crate::altd_server_signed_commits::require_signed_commits(&root.join("held"));
    let trust = root.join("held/.alt/trust");
    fs::create_dir(&trust).unwrap();
    let pipe = trust.join("slow.pub");
    let made = Command::new("mkfifo").arg(&pipe).status().unwrap();
    assert!(made.success());
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_ROOT", root.to_str().unwrap()),
        ("ALT_SERVER_MAX_PUSH_BYTES", "400000"),
        ("ALT_SERVER_MAX_INDEX_BYTES", "500000"),
        ("ALT_SERVER_MAX_INDEX_WAIT_MS", "300000"),
        ("ALT_SERVER_KEEPALIVE_MS", "200"),
    ]);
    let url = |name: &str| format!("http://{}/{name}/", server.addr);
    let large = source(tmp.path(), "large");
    commit(&large, "noise", 100_000);
    let by_git = source(tmp.path(), "by-git");
    commit(&by_git, "noise", 100_000);
    let by_alt = tmp.path().join("by-alt");
    ok(alt(tmp.path(), &["clone", &url("other"), "by-alt"]));
    std::fs::write(by_alt.join("noise"), noise(100_000)).unwrap();
    ok(alt(&by_alt, &["add", "."]));
    ok(alt(&by_alt, &["commit", "-m", "noise"]));

    let (reach, reached) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let (pushed_by_git, pushed_by_alt): (Output, Output) = std::thread::scope(|scope| {
        let release = Released(release);
        scope.spawn(|| push(&large, &url("held"), "large"));
        let to = pipe.clone();
        scope.spawn(move || {
            let written = fs::File::options().write(true).open(&to);
            reach.send(()).unwrap();
            let _ = released.recv();
            drop(written);
        });
        if reached.recv_timeout(Duration::from_secs(120)).is_err() {
            drop(fs::File::open(&pipe));
            panic!("the push never came to the keys it is checked against");
        }
        let by_git = scope.spawn(|| {
            let mut push = Command::new("git");
            push.current_dir(&by_git)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_HTTP_LOW_SPEED_LIMIT", "1")
                .env("GIT_HTTP_LOW_SPEED_TIME", "2")
                .args(["push", &url("other"), "HEAD:refs/heads/by-git"]);
            push.output().unwrap()
        });
        let by_alt = scope.spawn(|| alt(&by_alt, &["push", "origin", "main:refs/heads/by-alt"]));
        std::thread::sleep(Duration::from_secs(35));
        drop(release);
        (by_git.join().unwrap(), by_alt.join().unwrap())
    });
    let said = |out: &Output| String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(pushed_by_git.status.success(), "{}", said(&pushed_by_git));
    assert!(pushed_by_alt.status.success(), "{}", said(&pushed_by_alt));
    let refs = git(tmp.path(), &["ls-remote", &url("other")]);
    let refs = String::from_utf8_lossy(&refs.stdout).into_owned();
    assert!(refs.contains("refs/heads/by-git"), "{refs}");
    assert!(refs.contains("refs/heads/by-alt"), "{refs}");
}
