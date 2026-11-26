//! The memory the packs of pushes are indexed in is one amount for the
//! server: a push that finds it taken waits for it for as long as its
//! body was given to arrive, and is then told to come again.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::mpsc;
use std::time::Duration;

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_pack_limits::{git, push, source};
use crate::altd_server_untrusted::repository;

/// A commit in `src` of a file of `bytes` bytes that do not compress.
pub(crate) fn commit(src: &Path, name: &str, bytes: usize) {
    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ name.len() as u64;
    let noise: Vec<u8> = (0..bytes)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (state >> 56) as u8
        })
        .collect();
    fs::write(src.join(name), noise).unwrap();
    assert!(git(src, &["add", "."]).status.success());
    assert!(git(src, &["commit", "-q", "-m", name]).status.success());
}

#[test]
fn a_push_waits_for_the_memory_another_is_indexed_in_and_is_told_when_it_waited_too_long() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    repository(&root.join("held"));
    repository(&root.join("other"));
    // the commits of a push to `held` are checked against the keys it
    // trusts once its pack is indexed, and one of the keys is a pipe:
    // the push stands there, with its memory, until the pipe is written
    crate::altd_server_signed_commits::require_signed_commits(&root.join("held"));
    let trust = root.join("held/.alt/trust");
    fs::create_dir(&trust).unwrap();
    let pipe = trust.join("slow.pub");
    assert!(
        Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap()
            .success()
    );
    // room for one pack with an object of a hundred thousand bytes and no
    // second such pack beside it
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_ROOT", root.to_str().unwrap()),
        ("ALT_SERVER_MAX_PUSH_BYTES", "400000"),
        ("ALT_SERVER_MAX_INDEX_BYTES", "500000"),
        ("ALT_SERVER_MAX_INDEX_WAIT_MS", "10000"),
    ]);
    let url = |name: &str| format!("http://{}/{name}/", server.addr);
    let said = |out: &Output| String::from_utf8_lossy(&out.stderr).into_owned();
    let (large, small, waits) = (
        source(tmp.path(), "large"),
        source(tmp.path(), "second"),
        source(tmp.path(), "waits"),
    );
    commit(&large, "noise", 100_000);
    commit(&waits, "noise", 100_000);
    // as large: while it is scanned, room is kept free for all that one of
    // its entries can inflate to, and beside the first pack there is none
    commit(&small, "noise", 100_000);

    let (reach, reached) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let (refused, landed) = std::thread::scope(|scope| {
        scope.spawn(|| push(&large, &url("held"), "large"));
        // opening the pipe to write ends when the server has it open to read
        let to = pipe.clone();
        scope.spawn(move || {
            let written = fs::File::options().write(true).open(&to);
            reach.send(()).unwrap();
            let _ = released.recv();
            drop(written);
        });
        if reached.recv_timeout(Duration::from_secs(120)).is_err() {
            // lets the writer of the pipe go, and so the scope end
            drop(fs::File::open(&pipe));
            panic!("the push never came to the keys it is checked against");
        }
        // told to come again once it waited as long as a push waits
        let (tell, told) = mpsc::channel();
        scope.spawn(move || tell.send(push(&small, &url("other"), "small")));
        let Ok(refused) = told.recv_timeout(Duration::from_secs(60)) else {
            release.send(()).unwrap();
            panic!("a push waits for the memory another pack has without an end");
        };
        // one that still waits when the memory is given back lands
        let (tell, told) = mpsc::channel();
        scope.spawn(move || tell.send(push(&waits, &url("other"), "waits")));
        let early = told.recv_timeout(Duration::from_secs(2));
        release.send(()).unwrap();
        match early {
            Ok(early) => panic!("no wait for the memory another pack has: {early:?}"),
            Err(_) => (refused, told.recv_timeout(Duration::from_secs(120))),
        }
    });
    let told = said(&refused);
    // it waited for longer than the server waits before it sends the head of
    // the answer: it is told in the side band, as git shows it
    let again = "remote: the packs of other pushes are being indexed; send the push again";
    assert!(told.contains(again), "{told}");
    let landed = landed.expect("the push that waited did not end");
    assert!(landed.status.success(), "{}", said(&landed));
    // what was told to come again does so, and lands
    let again = push(&large, &url("other"), "again");
    assert!(again.status.success(), "{}", said(&again));
}

/// How many pushes the server has taken whole under `held`, where it keeps what
/// it writes for a time: the body of each, or the pack made of it, is a file of
/// a commit of 100,000 bytes of noise.
fn received(held: &std::path::Path) -> usize {
    let under = |directory: &std::path::Path| -> Vec<std::path::PathBuf> {
        let read = fs::read_dir(directory).into_iter().flatten();
        read.flatten().map(|entry| entry.path()).collect()
    };
    let requests = under(held).into_iter().flat_map(|server| under(&server));
    let whole = |file: &std::path::PathBuf| {
        let named = file.extension().is_none_or(|extension| extension == "pack");
        named && fs::metadata(file).is_ok_and(|file| file.len() >= 100_000)
    };
    requests
        .filter(|request| under(request).iter().any(whole))
        .count()
}

/// Lets the key file be read to its end when this is let go of, however the test
/// ends: the threads that wait for that are waited for themselves.
pub(crate) struct Released(pub(crate) mpsc::Sender<()>);

impl Drop for Released {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// A push waits for the memory another has, on one of two workers, and
/// the client of it goes away. The worker is there for the next request
/// long before the push would have waited to its end.
#[test]
fn a_push_that_waits_for_memory_keeps_no_worker_once_its_client_is_gone() {
    use std::os::unix::process::CommandExt;
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    repository(&root.join("held"));
    repository(&root.join("other"));
    crate::altd_server_signed_commits::require_signed_commits(&root.join("held"));
    let trust = root.join("held/.alt/trust");
    fs::create_dir(&trust).unwrap();
    let pipe = trust.join("slow.pub");
    let made = Command::new("mkfifo").arg(&pipe).status().unwrap();
    assert!(made.success());
    // where the server keeps the packs of pushes while it has them
    let held = tmp.path().join("held-by-the-server");
    fs::create_dir(&held).unwrap();
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_ROOT", root.to_str().unwrap()),
        ("ALT_SERVER_WORKERS", "2"),
        ("ALT_SERVER_MAX_PUSH_BYTES", "400000"),
        ("ALT_SERVER_MAX_INDEX_BYTES", "500000"),
        ("ALT_SERVER_MAX_INDEX_WAIT_MS", "300000"),
        ("ALT_SERVER_TEMP_DIR", held.to_str().unwrap()),
    ]);
    let url = |name: &str| format!("http://{}/{name}/", server.addr);
    let (large, waits) = (source(tmp.path(), "large"), source(tmp.path(), "waits"));
    commit(&large, "noise", 100_000);
    commit(&waits, "noise", 100_000);

    let (reach, reached) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let listed = std::thread::scope(|scope| {
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
        // the second push, in a group of its own: git and what it runs for it
        let mut waiting = Command::new("git")
            .current_dir(&waits)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["push", &url("other"), "HEAD:refs/heads/waits"])
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        // its pack is there whole beside that of the first: what comes next
        // for it is the memory the first has
        let began = std::time::Instant::now();
        while received(&held) < 2 {
            assert!(began.elapsed() < Duration::from_secs(120), "no second push");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            waiting.try_wait().unwrap().is_none(),
            "the push did not wait"
        );
        let group = format!("-{}", waiting.id());
        let killed = Command::new("kill").args(["-KILL", "--", &group]).status();
        assert!(killed.unwrap().success());
        waiting.wait().unwrap();
        // both workers were taken: this is answered by the one the push left
        let (tell, told) = mpsc::channel();
        let (other, from) = (url("other"), &large);
        scope.spawn(move || tell.send(git(from, &["ls-remote", &other])));
        let listed = told.recv_timeout(Duration::from_secs(60));
        drop(release);
        listed
    });
    let listed = listed.expect("the worker of a push nobody waits for was not given up");
    assert!(listed.status.success(), "{listed:?}");
}
