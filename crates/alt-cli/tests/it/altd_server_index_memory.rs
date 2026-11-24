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
fn commit(src: &Path, name: &str, bytes: usize) {
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
    assert!(told.contains("503"), "{told}");
    let landed = landed.expect("the push that waited did not end");
    assert!(landed.status.success(), "{}", said(&landed));
    // what was told to come again does so, and lands
    let again = push(&large, &url("other"), "again");
    assert!(again.status.success(), "{}", said(&again));
}
