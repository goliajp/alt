//! A push whose pack takes long to take in hears from the server meanwhile:
//! the head of the answer and keepalives, as `git receive-pack` sends them.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::altd_server::{Server, alt, ok, spawn_server_with_env};
use crate::altd_server_index_memory::{Released, commit};
use crate::altd_server_pack_limits::{git, push, source};
use crate::altd_server_slow_peers::noise;
use crate::altd_server_untrusted::repository;

/// A server of the repositories `held` and `other` under `root`, whose
/// memory for packs has room for one push at a time, and the key of `held`
/// that a push to it stands on until the key is written: a pipe.
fn served(root: &Path) -> (Server, PathBuf) {
    repository(&root.join("held"));
    repository(&root.join("other"));
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
    (server, pipe)
}

/// What `meanwhile` starts while a push of `large` to `held` stands on
/// `pipe` with the memory, which it then lets go of.
fn while_one_stands<T>(
    pipe: &Path,
    (large, held): (&Path, &str),
    meanwhile: impl FnOnce() -> T,
) -> T {
    let (reach, reached) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let release = Released(release);
        scope.spawn(|| push(large, held, "large"));
        let to = pipe.to_owned();
        scope.spawn(move || {
            let written = fs::File::options().write(true).open(&to);
            reach.send(()).unwrap();
            let _ = released.recv();
            drop(written);
        });
        if reached.recv_timeout(Duration::from_secs(120)).is_err() {
            drop(fs::File::open(pipe));
            panic!("the push never came to the keys it is checked against");
        }
        let started = meanwhile();
        drop(release);
        started
    })
}

/// `git push` of the head of `src` to `url` as `branch`, started.
fn git_push(src: &Path, url: String, branch: &str, env: &[(&str, &str)]) -> JoinHandle<Output> {
    let mut push = Command::new("git");
    push.current_dir(src)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(env.iter().copied())
        .args(["push", &url, &format!("HEAD:refs/heads/{branch}")]);
    std::thread::spawn(move || push.output().unwrap())
}

/// Two pushes wait for the memory a third is indexed in for longer than
/// their clients let a connection be silent: `alt`, thirty seconds, and
/// `git` told to give up after two seconds of less than a byte a second.
/// They hear keepalives meanwhile, and land once the memory is theirs.
#[test]
fn a_push_that_waits_longer_than_its_client_hears_nothing_for_lands() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    let (server, pipe) = served(&root);
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

    let (pushed_by_git, pushed_by_alt) = while_one_stands(&pipe, (&large, &url("held")), || {
        let slow = [
            ("GIT_HTTP_LOW_SPEED_LIMIT", "1"),
            ("GIT_HTTP_LOW_SPEED_TIME", "2"),
        ];
        let pushed_by_git = git_push(&by_git, url("other"), "by-git", &slow);
        let pushed_by_alt =
            std::thread::spawn(move || alt(&by_alt, &["push", "origin", "main:refs/heads/by-alt"]));
        std::thread::sleep(Duration::from_secs(35));
        (pushed_by_git, pushed_by_alt)
    });
    let (pushed_by_git, pushed_by_alt) = (pushed_by_git.join(), pushed_by_alt.join());
    let (pushed_by_git, pushed_by_alt) = (pushed_by_git.unwrap(), pushed_by_alt.unwrap());
    let said = |out: &Output| String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(pushed_by_git.status.success(), "{}", said(&pushed_by_git));
    assert!(pushed_by_alt.status.success(), "{}", said(&pushed_by_alt));
    let refs = git(tmp.path(), &["ls-remote", &url("other")]);
    let refs = String::from_utf8_lossy(&refs.stdout).into_owned();
    assert!(refs.contains("refs/heads/by-git"), "{refs}");
    assert!(refs.contains("refs/heads/by-alt"), "{refs}");
}

/// A push whose head went out and that fails after it is told why in the
/// side band of errors, without a word before the reason: the keys of the
/// repository are not there to be read once it has the memory.
#[test]
fn a_push_that_fails_once_its_head_went_out_is_told_why_in_the_side_band() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    let (server, pipe) = served(&root);
    crate::altd_server_signed_commits::require_signed_commits(&root.join("other"));
    let url = |name: &str| format!("http://{}/{name}/", server.addr);
    let large = source(tmp.path(), "large");
    commit(&large, "noise", 100_000);
    let waits = source(tmp.path(), "waits");
    commit(&waits, "noise", 100_000);

    let pushed = while_one_stands(&pipe, (&large, &url("held")), || {
        let pushed = git_push(&waits, url("other"), "waits", &[]);
        // ten keepalives and more: the head of its answer went out
        std::thread::sleep(Duration::from_secs(2));
        fs::write(root.join("other/.alt/trust"), "").unwrap();
        pushed
    });
    let pushed = pushed.join().unwrap();
    let said = String::from_utf8_lossy(&pushed.stderr).into_owned();
    assert!(!pushed.status.success(), "{said}");
    // git shows an error of a push in the side band as it shows the progress
    assert!(said.contains("remote: Not a directory"), "{said}");
}
