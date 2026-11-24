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
pub(crate) fn listed(url: &str) -> Option<String> {
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

/// bytes no compression makes fewer
pub(crate) fn noise(count: usize) -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 24) as u8
    };
    (0..count).map(|_| next()).collect()
}

/// sixteen hundred bytes ten times a second: a line of 128 kbit/s
const PIECE: usize = 1600;
const REST: Duration = Duration::from_millis(100);

/// A push comes over a slow line, at an even pace, with the times peers are given
/// as they are by default: it goes through.
///
/// The scale is that of a megabit a second. Peers were once given eight seconds
/// for each MiB they moved and twenty beside, which a megabit a second falls
/// behind of after 432 seconds, at 54 MB, and this line after 23 seconds, at 370
/// thousand bytes. Here 700 thousand bytes take 44 seconds, which is into the
/// second of the windows a body has to move in.
#[test]
fn a_push_over_a_slow_line_goes_through_with_the_times_peers_are_given_by_default() {
    use crate::altd_server_pack_limits::{git, push, source};
    use crate::altd_server_relay::{Relayed, relay};
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let server = spawn_server_with_env(&[("ALT_SERVER_REPO", origin.to_str().unwrap())]);
    let slow = Relayed {
        pace: Some((PIECE, REST)),
        ..Default::default()
    };
    let through = relay(&server.addr, slow);
    let src = source(tmp.path(), "slow");
    std::fs::write(src.join("noise"), noise(700_000)).unwrap();
    assert!(git(&src, &["add", "."]).status.success());
    assert!(git(&src, &["commit", "-q", "-m", "noise"]).status.success());
    let began = std::time::Instant::now();
    let pushed = push(&src, &format!("http://{through}/"), "slow");
    let taken = began.elapsed();
    let said = String::from_utf8_lossy(&pushed.stderr).into_owned();
    assert!(pushed.status.success(), "after {taken:?}: {said}");
    assert!(taken >= Duration::from_secs(40), "{taken:?}");
    let refs = listed(&format!("http://{}/", server.addr)).unwrap();
    assert!(refs.contains("refs/heads/slow"), "{refs}");
}

/// A clone is read over the same slow line with the default times: after 75
/// seconds of it the server still sends, and the rest, read as fast as it comes,
/// is whole. What the sockets hold between them goes out at once, so the first
/// of the windows a response has to move in tells nothing; the second is all at
/// the pace of the line, and the 75 seconds reach into the third.
#[test]
fn a_clone_read_over_a_slow_line_is_sent_whole_with_the_times_peers_are_given_by_default() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    // more than the sockets of the connection hold between them
    const BYTES: usize = 8 << 20;
    std::fs::write(origin.join("noise"), noise(BYTES)).unwrap();
    let alt = crate::altd_server::alt;
    crate::altd_server::ok(alt(&origin, &["add", "."]));
    crate::altd_server::ok(alt(&origin, &["commit", "-m", "noise"]));
    let server = spawn_server_with_env(&[("ALT_SERVER_REPO", origin.to_str().unwrap())]);
    let refs = listed(&format!("http://{}/", server.addr)).unwrap();
    let head = refs.split_whitespace().next().unwrap();
    let pkt = |line: &str| format!("{:04x}{line}", line.len() + 4);
    let command = [
        pkt("command=fetch\n"),
        "0001".to_owned(),
        pkt(&format!("want {head}\n")),
        pkt("done\n"),
        "0000".to_owned(),
    ]
    .concat();
    let request = format!(
        "POST /git-upload-pack HTTP/1.1\r\nHost: h\r\nGit-Protocol: version=2\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{command}",
        command.len()
    );
    let mut socket = TcpStream::connect(&server.addr).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(120)))
        .unwrap();
    socket.write_all(request.as_bytes()).unwrap();
    let mut piece = vec![0u8; PIECE];
    let mut read = 0;
    let began = std::time::Instant::now();
    while began.elapsed() < Duration::from_secs(75) {
        let count = socket.read(&mut piece).unwrap();
        assert_ne!(
            count,
            0,
            "ended after {:?} and {read} bytes",
            began.elapsed()
        );
        read += count;
        std::thread::sleep(REST);
    }
    assert!(read < BYTES / 4, "{read}");
    let mut rest = Vec::new();
    socket.read_to_end(&mut rest).unwrap();
    assert!(read + rest.len() > BYTES, "{} bytes", read + rest.len());
}

/// A body whose chunks are not framed as chunks is a request that is not
/// understood, not one that is too large.
#[test]
fn a_body_of_chunks_that_are_none_is_answered_as_a_bad_request() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let server = spawn_server_with_env(&[("ALT_SERVER_REPO", origin.to_str().unwrap())]);
    let mut socket = TcpStream::connect(&server.addr).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let request = "POST /git-upload-pack HTTP/1.1\r\nHost: h\r\nGit-Protocol: version=2\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nnot a size\r\n";
    socket.write_all(request.as_bytes()).unwrap();
    let mut told = String::new();
    socket.read_to_string(&mut told).unwrap();
    assert!(told.starts_with("HTTP/1.1 400 "), "{told}");
}
