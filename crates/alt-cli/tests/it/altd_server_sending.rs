//! The room the responses to fetches have on the disk between them.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_slow_peers::{listed, noise};
use crate::altd_server_untrusted::repository;

/// A connection that has asked `address` for the whole of the commit `head`.
pub(crate) fn asked(address: &str, head: &str) -> TcpStream {
    asked_for(address, head, None)
}

/// The same, where `from` as a proxy that says whose the request is.
fn asked_for(address: &str, head: &str, from: Option<&str>) -> TcpStream {
    let pkt = |line: &str| format!("{:04x}{line}", line.len() + 4);
    let want = pkt(&format!("want {head}\n"));
    let command = [
        &pkt("command=fetch\n"),
        "0001",
        &want,
        &pkt("done\n"),
        "0000",
    ]
    .concat();
    let from = from.map_or(String::new(), |from| format!("X-Forwarded-For: {from}\r\n"));
    let request = format!(
        "POST /git-upload-pack HTTP/1.1\r\nHost: h\r\n{from}Git-Protocol: version=2\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{command}",
        command.len()
    );
    let mut socket = TcpStream::connect(address).unwrap();
    let wait = Some(Duration::from_secs(60));
    socket.set_read_timeout(wait).unwrap();
    socket.write_all(request.as_bytes()).unwrap();
    socket
}

pub(crate) fn answer(mut socket: TcpStream) -> Vec<u8> {
    let mut all = Vec::new();
    socket.read_to_end(&mut all).unwrap();
    all
}

#[test]
fn a_fetch_whose_response_has_no_room_beside_one_being_sent_is_told_to_come_again() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    // more than the sockets of a connection hold between them
    const BYTES: usize = 16 << 20;
    std::fs::write(origin.join("noise"), noise(BYTES)).unwrap();
    let alt = crate::altd_server::alt;
    crate::altd_server::ok(alt(&origin, &["add", "."]));
    crate::altd_server::ok(alt(&origin, &["commit", "-m", "noise"]));
    // less room than one response takes: alone it is sent all the same
    // the head of a response goes out once its pack is made: one read
    // from then on holds its room, and one refused is a 503. A head sent
    // ahead of a pack slow to make would come before either
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_MAX_SENDING_BYTES", "1000000"),
        ("ALT_SERVER_KEEPALIVE_MS", "3600000"),
    ]);
    let refs = listed(&format!("http://{}/", server.addr)).unwrap();
    let head = refs.split_whitespace().next().unwrap();

    // the first response is begun, so it is written, and no more of it is taken
    let mut first = asked(&server.addr, head);
    let mut begun = [0u8; 12];
    first.read_exact(&mut begun).unwrap();
    assert_eq!(&begun, b"HTTP/1.1 200");

    let second = String::from_utf8_lossy(&answer(asked(&server.addr, head))).into_owned();
    assert!(second.starts_with("HTTP/1.1 503"), "{second:.200}");
    assert!(second.contains("\r\nRetry-After: 60\r\n"), "{second:.400}");
    assert!(second.contains("fetch again"), "{second:.400}");

    // the first is given up by its peer: its room is there again
    drop(first);
    let until = Instant::now() + Duration::from_secs(60);
    let third = loop {
        let third = answer(asked(&server.addr, head));
        if third.starts_with(b"HTTP/1.1 200") {
            break third;
        }
        assert!(
            Instant::now() < until,
            "the room of a response given up is kept"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(third.len() > BYTES, "{} bytes", third.len());
}

/// The room is eighty megabytes, of which an address has twenty. One address
/// lets a response of sixteen wait: a second fetch of its own is told to come
/// again, while one from this host is answered whole. The address is the one a
/// proxy on this host says its requests are of.
#[test]
fn the_responses_one_address_lets_wait_leave_the_room_of_the_others() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    const BYTES: usize = 16 << 20;
    std::fs::write(origin.join("noise"), noise(BYTES)).unwrap();
    let alt = crate::altd_server::alt;
    crate::altd_server::ok(alt(&origin, &["add", "."]));
    crate::altd_server::ok(alt(&origin, &["commit", "-m", "noise"]));
    // as above: the head goes out once the pack is made
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_MAX_SENDING_BYTES", "80000000"),
        ("ALT_SERVER_KEEPALIVE_MS", "3600000"),
    ]);
    let refs = listed(&format!("http://{}/", server.addr)).unwrap();
    let head = refs.split_whitespace().next().unwrap();
    let far = || asked_for(&server.addr, head, Some("203.0.113.9"));

    let mut first = far();
    let mut begun = [0u8; 12];
    first.read_exact(&mut begun).unwrap();
    assert_eq!(&begun, b"HTTP/1.1 200");
    let second = String::from_utf8_lossy(&answer(far())).into_owned();
    assert!(second.starts_with("HTTP/1.1 503"), "{second:.200}");
    assert!(second.contains("fetch again"), "{second:.400}");
    let other = answer(asked(&server.addr, head));
    assert!(other.starts_with(b"HTTP/1.1 200") && other.len() > BYTES);

    drop(first);
    let until = Instant::now() + Duration::from_secs(60);
    while !answer(far()).starts_with(b"HTTP/1.1 200") {
        assert!(Instant::now() < until, "the room of an address is kept");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The same room, and a server told that its proxy is at another address:
/// this host is no proxy then. What a peer of it says its requests are of
/// is not read, so the response it lets wait takes nothing of what the
/// address it names has, and a second one is answered whole beside it.
#[test]
fn what_a_peer_that_is_no_proxy_says_its_requests_are_of_is_not_read() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    const BYTES: usize = 16 << 20;
    std::fs::write(origin.join("noise"), noise(BYTES)).unwrap();
    let alt = crate::altd_server::alt;
    crate::altd_server::ok(alt(&origin, &["add", "."]));
    crate::altd_server::ok(alt(&origin, &["commit", "-m", "noise"]));
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_MAX_SENDING_BYTES", "80000000"),
        ("ALT_SERVER_TRUSTED_PROXIES", "192.0.2.1"),
    ]);
    let refs = listed(&format!("http://{}/", server.addr)).unwrap();
    let head = refs.split_whitespace().next().unwrap();
    let named = || asked_for(&server.addr, head, Some("203.0.113.9"));

    let mut first = named();
    let mut begun = [0u8; 12];
    first.read_exact(&mut begun).unwrap();
    assert_eq!(&begun, b"HTTP/1.1 200");
    let second = answer(named());
    assert!(second.starts_with(b"HTTP/1.1 200") && second.len() > BYTES);
}
