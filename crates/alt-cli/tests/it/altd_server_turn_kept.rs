//! A fetch whose large response waits for its turn hears from the server
//! meanwhile, and what went out ahead of its pack still holds once the pack
//! is made again in its turn.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::altd_server::{alt, ok, spawn_server_with_env};
use crate::altd_server_keepalive::{chunked, lines};
use crate::altd_server_sending::answer;
use crate::altd_server_slow_peers::{listed, noise};
use crate::altd_server_untrusted::repository;

/// A fetch of `head` from `from` that has `have` and does not say `done`:
/// its answer acknowledges `have` ahead of the pack.
fn having(address: &str, (head, have): (&str, &str), from: &str) -> TcpStream {
    let pkt = |line: String| format!("{:04x}{line}", line.len() + 4);
    let command = [
        pkt("command=fetch\n".into()),
        "0001".into(),
        pkt(format!("want {head}\n")),
        pkt(format!("have {have}\n")),
        "0000".into(),
    ]
    .concat();
    let request = format!(
        "POST /git-upload-pack HTTP/1.1\r\nHost: h\r\nX-Forwarded-For: {from}\r\nGit-Protocol: version=2\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{command}",
        command.len()
    );
    let mut socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(120)))
        .unwrap();
    socket.write_all(request.as_bytes()).unwrap();
    socket
}

/// The pkt-lines of an answer sent in chunks but its keepalives, and how
/// many of those there were.
fn kept_out(answer: &[u8]) -> (Vec<Option<Vec<u8>>>, usize) {
    let (head, body) = chunked(answer);
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    let all = lines(&body);
    let beats = all.iter().filter(|l| **l == Some(&b"\x01"[..])).count();
    let rest = all.into_iter().filter(|l| *l != Some(&b"\x01"[..]));
    (rest.map(|l| l.map(<[u8]>::to_vec)).collect(), beats)
}

/// Two workers, so one turn, and a repository whose response is large. One
/// address keeps the turn, its pack coming and unread; another fetches with
/// a commit it has, and its head and acknowledgments go out with keepalives
/// behind them while its pack is made, found large, and waits for the turn.
/// Once the turn is given back the pack is made again: the answer has one
/// head, its acknowledgments once, and the same lines as the answer to that
/// fetch alone.
#[test]
fn a_fetch_that_waits_for_its_turn_once_its_head_went_out_is_answered_behind_that_head() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let seed = ok(alt(&origin, &["rev-parse", "HEAD"])).trim().to_owned();
    // more than a quarter of the room of sixty megabytes
    std::fs::write(origin.join("noise"), noise(16 << 20)).unwrap();
    ok(alt(&origin, &["add", "."]));
    ok(alt(&origin, &["commit", "-m", "noise"]));
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_WORKERS", "2"),
        ("ALT_SERVER_MAX_SENDING_BYTES", "60000000"),
        ("ALT_SERVER_MAX_FETCH_MS", "120000"),
        ("ALT_SERVER_KEEPALIVE_MS", "1"),
    ]);
    let refs = listed(&format!("http://{}/", server.addr)).unwrap();
    let head = refs.split_whitespace().next().unwrap().to_owned();
    let asked = (head.as_str(), seed.as_str());

    let began = Instant::now();
    let (alone, _) = kept_out(&answer(having(&server.addr, asked, "203.0.113.8")));
    let took = began.elapsed();
    assert_eq!(alone[0].as_deref(), Some(&b"acknowledgments\n"[..]));
    assert_eq!(alone[1], Some(format!("ACK {seed}\n").into_bytes()));

    // the turn is kept once the pack of the first has begun to come
    let mut first = having(&server.addr, asked, "203.0.113.9");
    let (mut read, mut piece) = (Vec::new(), [0u8; 4096]);
    while !read.windows(4).any(|w| w == b"PACK") {
        let count = first.read(&mut piece).unwrap();
        assert!(count > 0, "{:.300}", String::from_utf8_lossy(&read));
        read.extend_from_slice(&piece[..count]);
    }
    let waiter = {
        let address = server.addr.clone();
        let asked = (head.clone(), seed.clone());
        let from = "203.0.113.10";
        std::thread::spawn(move || answer(having(&address, (&asked.0, &asked.1), from)))
    };
    // its pack is made and found large by then
    std::thread::sleep(took * 2 + Duration::from_secs(2));
    assert!(!waiter.is_finished(), "answered while the turn was kept");
    drop(first);
    let (answered, beats) = kept_out(&waiter.join().unwrap());
    assert!(beats > 0, "no keepalive");
    assert!(answered == alone, "{:?}", &answered[..3]);
}
