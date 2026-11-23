//! Peers of `altd-server` that send nothing, or less than they said: none
//! of them keeps a worker from the others for longer than a peer is given.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::sync::mpsc::channel;
use std::time::Duration;

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_untrusted::repository;

/// The refs `git ls-remote` lists at `url`, or nothing where it has not
/// answered in a minute.
fn listed(url: &str) -> Option<String> {
    let (done, waited) = channel();
    let url = url.to_owned();
    std::thread::spawn(move || {
        let out = Command::new("git")
            .args(["ls-remote", &url])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        let told = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "{told}");
        let _ = done.send(String::from_utf8(out.stdout).unwrap());
    });
    waited.recv_timeout(Duration::from_secs(60)).ok()
}

#[test]
fn a_peer_whose_body_does_not_come_keeps_the_one_worker_for_no_longer_than_it_is_given() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_WORKERS", "1"),
        ("ALT_SERVER_IO_TIMEOUT_MS", "500"),
        ("ALT_SERVER_IO_WINDOW_MS", "500"),
    ]);
    let url = format!("http://{}/", server.addr);
    // two that send no head, and one whose request the worker takes up:
    // it says a body of some thousand bytes follows and sends none
    let silent = [(); 2].map(|()| TcpStream::connect(&server.addr).unwrap());
    let mut stalled = TcpStream::connect(&server.addr).unwrap();
    let head = "POST /git-upload-pack HTTP/1.1\r\nHost: h\r\nContent-Length: 4096\r\n\r\n";
    stalled.write_all(head.as_bytes()).unwrap();

    let refs = listed(&url).expect("the worker is kept by a peer that sends nothing");
    assert!(refs.contains("refs/heads/main"), "{refs}");
    // the peer is told that it took too long, and its connection is ended
    stalled
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut told = String::new();
    stalled.read_to_string(&mut told).unwrap();
    assert!(told.starts_with("HTTP/1.1 408 "), "{told}");
    assert!(told.contains("\r\nConnection: close\r\n"), "{told}");
    for mut peer in silent {
        peer.set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }
}

/// A body that comes slowly and goes on coming is not given up, and
/// keeps the one worker from nobody while it comes: it is received by
/// its connection. Sent so, all of it would take some minutes.
#[test]
fn a_body_that_comes_slowly_and_goes_on_keeps_the_one_worker_from_nobody() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_WORKERS", "1"),
        ("ALT_SERVER_IO_WINDOW_MS", "500"),
    ]);
    let url = format!("http://{}/", server.addr);
    let mut slow = TcpStream::connect(&server.addr).unwrap();
    let head = "POST /git-upload-pack HTTP/1.1\r\nHost: h\r\nContent-Length: 4000000\r\n\r\n";
    slow.write_all(head.as_bytes()).unwrap();
    let (stop, stopped) = channel::<()>();
    let sender = std::thread::spawn(move || {
        // two thousand bytes each tenth of a second: forty times what a
        // body has to move
        let mut sent = 0usize;
        while stopped.try_recv().is_err() && slow.write_all(&[b'0'; 2000]).is_ok() {
            sent += 2000;
            std::thread::sleep(Duration::from_millis(100));
        }
        sent
    });
    let first = listed(&url);
    std::thread::sleep(Duration::from_secs(2));
    let later = listed(&url);
    stop.send(()).unwrap();
    let sent = sender.join().unwrap();
    assert!(first.is_some_and(|refs| refs.contains("refs/heads/main")));
    assert!(later.is_some_and(|refs| refs.contains("refs/heads/main")));
    // four windows and more went by, and the body was still taken
    assert!(sent >= 40_000, "{sent}");
}
