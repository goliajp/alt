//! How the shim ends what the service it runs answers: with the last chunk
//! where the service ended well, cut short where it failed, and the service
//! ended where the client went away.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use super::held::{git, pkt};

/// A repository of git with one commit of a file of `bytes` bytes that do
/// not compress, and its head.
fn repository(repo: &Path, bytes: usize) -> String {
    std::fs::create_dir(repo).unwrap();
    git(repo, &["init", "-q"]);
    let noise = crate::altd_server_slow_peers::noise(bytes);
    std::fs::write(repo.join("noise"), noise).unwrap();
    git(repo, &["add", "noise"]);
    let who = ["-c", "user.name=t", "-c", "user.email=t@e"];
    git(repo, &[&who[..], &["commit", "-qm", "noise"]].concat());
    git(repo, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// A script at `path` that runs `body` with the real git at `$git`.
fn script(path: &Path, body: &str) {
    let found = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8(found.stdout).unwrap();
    let text = format!("#!/bin/sh\ngit='{}'\n{body}", real.trim());
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// git, run by the shim, writes its whole answer to the fetch of a pack and
/// then fails: the fetch fails as the answer of a failed service does,
/// though nothing of the answer is missing
#[test]
fn a_service_that_fails_behind_its_whole_answer_fails_the_fetch() {
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path().join("repo");
    repository(&repo, 1000);
    let wrapped = dir.path().join("wrapped");
    std::fs::create_dir(&wrapped).unwrap();
    // the other requests of the clone are answered as git answers them
    script(
        &wrapped.join("git"),
        "input=$(mktemp)\ncat > \"$input\"\n\"$git\" \"$@\" < \"$input\"\nended=$?\n\
         if grep -q command=fetch \"$input\"; then ended=1; fi\nrm -f \"$input\"\nexit $ended\n",
    );
    let path = std::env::var("PATH").unwrap();
    let path = format!("{}:{path}", wrapped.display());
    let url = super::spawn_with_env(repo, vec![("PATH".to_owned(), path)]);
    let cloned = crate::altd_server::alt(dir.path(), &["clone", &url, "copy"]);
    let said = String::from_utf8_lossy(&cloned.stderr);
    assert!(!cloned.status.success(), "{said}");
}

/// A fetch whose answer is cut short in the middle of its pack fails and
/// leaves nothing of the pack behind in the repository
#[test]
fn a_fetch_cut_short_in_its_pack_leaves_no_part_of_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path().join("repo");
    repository(&repo, 1000);
    let (wrapped, cut) = (dir.path().join("wrapped"), dir.path().join("cut"));
    std::fs::create_dir(&wrapped).unwrap();
    // once `cut` is there, the answer to a fetch ends in the pack
    let cuts = format!(
        "input=$(mktemp)\ncat > \"$input\"\n\
         if [ -e '{}' ] && grep -q command=fetch \"$input\"; then\n\
         \"$git\" \"$@\" < \"$input\" | head -c 65536\nrm -f \"$input\"\nexit 1\nfi\n\
         \"$git\" \"$@\" < \"$input\"\nended=$?\nrm -f \"$input\"\nexit $ended\n",
        cut.display()
    );
    script(&wrapped.join("git"), &cuts);
    let path = std::env::var("PATH").unwrap();
    let path = format!("{}:{path}", wrapped.display());
    let url = super::spawn_with_env(repo.clone(), vec![("PATH".to_owned(), path)]);
    crate::altd_server::ok(crate::altd_server::alt(
        dir.path(),
        &["clone", &url, "copy"],
    ));
    let copy = dir.path().join("copy");
    let packs = copy.join(".alt/pack");
    // what is under the directory of packs, which a clone may not make
    let listed = || {
        let mut names: Vec<_> = match std::fs::read_dir(&packs) {
            Ok(entries) => entries.map(|entry| entry.unwrap().file_name()).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => panic!("{e}"),
        };
        names.sort();
        names
    };
    let before = listed();

    let noise = crate::altd_server_slow_peers::noise(1 << 20);
    std::fs::write(repo.join("more"), noise).unwrap();
    git(&repo, &["add", "more"]);
    let who = ["-c", "user.name=t", "-c", "user.email=t@e"];
    git(&repo, &[&who[..], &["commit", "-qm", "more"]].concat());
    std::fs::write(&cut, "").unwrap();
    let fetched = crate::altd_server::alt(&copy, &["fetch", "origin"]);
    let said = String::from_utf8_lossy(&fetched.stderr);
    assert!(!fetched.status.success(), "{said}");
    assert_eq!(listed(), before, "{said}");
}

/// A client that goes away while git makes its pack leaves no git behind
#[test]
fn a_client_that_goes_away_leaves_no_service_behind() {
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path().join("repo");
    // far more than the pipe from git holds
    let head = repository(&repo, 4 << 20);
    let (pid, gate) = (dir.path().join("pid"), dir.path().join("gate"));
    let hook = dir.path().join("hook");
    let waits = format!(
        "echo $PPID > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.05; done\nexec \"$@\"\n",
        pid.display(),
        gate.display()
    );
    script(&hook, &waits);
    let env = [
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "uploadpack.packObjectsHook"),
        ("GIT_CONFIG_VALUE_0", hook.to_str().unwrap()),
    ];
    let env = env.map(|(k, v)| (k.to_owned(), v.to_owned())).to_vec();
    let url = super::spawn_with_env(repo, env);

    let mut body = Vec::new();
    pkt(&mut body, "command=fetch\n");
    body.extend_from_slice(b"0001");
    pkt(&mut body, &format!("want {head}\n"));
    pkt(&mut body, "done\n");
    body.extend_from_slice(b"0000");
    let mut stream = TcpStream::connect(url.strip_prefix("http://").unwrap()).unwrap();
    let request = format!(
        "POST /git-upload-pack HTTP/1.1\r\nHost: h\r\nGit-Protocol: version=2\r\n\
         Content-Type: application/x-git-upload-pack-request\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(&body).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut status = String::new();
    BufReader::new(&stream).read_line(&mut status).unwrap();
    assert_eq!(status, "HTTP/1.1 200 OK\r\n");
    let until = Instant::now() + Duration::from_secs(60);
    let pid = loop {
        let told = std::fs::read_to_string(&pid).unwrap_or_default();
        if told.ends_with('\n') {
            break told.trim().to_owned();
        }
        assert!(Instant::now() < until, "pack-objects never started");
        std::thread::sleep(Duration::from_millis(50));
    };
    drop(stream);
    std::fs::write(&gate, "").unwrap();

    let until = Instant::now() + Duration::from_secs(60);
    let there = || {
        Command::new("kill")
            .args(["-0", &pid])
            .status()
            .unwrap()
            .success()
    };
    while there() {
        assert!(Instant::now() < until, "git upload-pack ({pid}) is left");
        std::thread::sleep(Duration::from_millis(100));
    }
}
