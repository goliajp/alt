//! A push whose pack takes long to take in hears from the server meanwhile:
//! the head of the answer and keepalives, as `git receive-pack` sends them.

use std::fs;
use std::process::{Command, Output};
use std::sync::mpsc;
use std::time::Duration;

use crate::altd_server::{alt, ok, spawn_server_with_env};
use crate::altd_server_slow_peers::noise;
use crate::altd_server_untrusted::repository;

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
