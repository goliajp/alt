//! Responses to fetches larger than the part of an address of the room: one of
//! an address at a time, half as many as there are workers at once, and the
//! others waiting for their turn for as long as a fetch may take. One kept by a
//! peer that takes it slowly is let go of once another waits for its turn.

use std::io::Read;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::altd_server::{Server, spawn_server_with_env};
use crate::altd_server_sending::{answer, asked_for};
use crate::altd_server_slow_peers::{listed, noise};
use crate::altd_server_untrusted::repository;

/// more than a quarter of the room of sixty megabytes, and less than half
const BYTES: usize = 16 << 20;
/// the time over which the pace of a large response is counted
const LARGE_WINDOW: Duration = Duration::from_secs(30);

/// A server of a repository of [`BYTES`] of noise, with `env`, and its head.
fn served(origin: &std::path::Path, env: &[(&str, &str)]) -> (Server, String) {
    repository(origin);
    std::fs::write(origin.join("noise"), noise(BYTES)).unwrap();
    let alt = crate::altd_server::alt;
    crate::altd_server::ok(alt(origin, &["add", "."]));
    crate::altd_server::ok(alt(origin, &["commit", "-m", "noise"]));
    let mut told = vec![
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_MAX_SENDING_BYTES", "60000000"),
    ];
    told.extend_from_slice(env);
    let server = spawn_server_with_env(&told);
    let refs = listed(&format!("http://{}/", server.addr)).unwrap();
    let head = refs.split_whitespace().next().unwrap().to_owned();
    (server, head)
}

/// A fetch from `from` whose answer has begun to come: it is read no further.
fn kept(server: &Server, head: &str, from: &str) -> TcpStream {
    let mut kept = asked_for(&server.addr, head, Some(from));
    let mut begun = [0u8; 12];
    kept.read_exact(&mut begun).unwrap();
    assert_eq!(&begun, b"HTTP/1.1 200");
    kept
}

/// A fetch from `from` on a thread of its own: all that is answered.
fn waiting(server: &Server, head: &str, from: &'static str) -> std::thread::JoinHandle<Vec<u8>> {
    let (address, head) = (server.addr.clone(), head.to_owned());
    std::thread::spawn(move || answer(asked_for(&address, &head, Some(from))))
}

/// Four workers, so two turns. A second fetch of an address waits while the
/// first is kept, though a turn is free, and one of another address is
/// answered meanwhile. The fetches may take as long as the answer to another
/// does on a busy host: that the one that waits is still waiting is what is
/// looked at, not how long it may.
#[test]
fn a_large_response_waits_for_its_turn_while_another_address_is_answered() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let env = [
        ("ALT_SERVER_WORKERS", "4"),
        ("ALT_SERVER_MAX_FETCH_MS", "120000"),
    ];
    let (server, head) = served(&tmp.path().join("origin"), &env);
    let (one, other) = ("203.0.113.9", "203.0.113.10");

    let first = kept(&server, &head, one);
    let second = waiting(&server, &head, one);
    std::thread::sleep(Duration::from_secs(1));
    let began = Instant::now();
    let beside = answer(asked_for(&server.addr, &head, Some(other)));
    assert!(
        beside.starts_with(b"HTTP/1.1 200") && beside.len() > BYTES,
        "{} bytes after {:?}: {:.300}",
        beside.len(),
        began.elapsed(),
        String::from_utf8_lossy(&beside)
    );
    assert!(
        !second.is_finished(),
        "answered after {:?}",
        began.elapsed()
    );
    drop(first);
    let second = second.join().unwrap();
    assert!(
        second.starts_with(b"HTTP/1.1 200"),
        "{:.200}",
        String::from_utf8_lossy(&second)
    );
    assert_eq!(second.len(), beside.len());
}

/// Two turns, both kept: a fetch that waits for one is told to come again
/// when the time a fetch may take is over, and not before. The responses that
/// keep the turns are larger than what the sockets hold, so that they keep
/// them; the time is long enough for them to be written on a busy host first.
#[test]
fn a_large_response_that_waits_while_every_turn_is_kept_is_told_to_come_again_when_its_time_is_over()
 {
    let tmp = alt_os::temp::tempdir().unwrap();
    let env = [
        ("ALT_SERVER_WORKERS", "4"),
        ("ALT_SERVER_MAX_FETCH_MS", "20000"),
    ];
    let (server, head) = served(&tmp.path().join("origin"), &env);
    let (one, other, third) = ("203.0.113.9", "203.0.113.10", "203.0.113.11");
    let (_first, _beside) = (kept(&server, &head, one), kept(&server, &head, other));
    let began = Instant::now();
    let late = answer(asked_for(&server.addr, &head, Some(third)));
    assert!(
        late.starts_with(b"HTTP/1.1 503"),
        "{:.200}",
        String::from_utf8_lossy(&late)
    );
    assert!(
        began.elapsed() >= Duration::from_secs(19),
        "{:?}",
        began.elapsed()
    );
}

/// The bytes of a response kept from the start, read at a quarter of a
/// megabyte a second until it ends, and how long that took.
fn read_slowly(mut kept: TcpStream) -> (usize, Duration) {
    let (mut read, mut piece) = (12, vec![0u8; 64 << 10]);
    let began = Instant::now();
    loop {
        match kept.read(&mut piece) {
            Ok(0) | Err(_) => return (read, began.elapsed()),
            Ok(count) => read += count,
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Two workers, so one turn. The response kept in it is read at a quarter of
/// a megabyte a second while another waits: it is let go of before it is
/// whole, at the end of the first half minute it is counted over, and the one
/// that waits has its turn.
#[test]
fn a_large_response_taken_slowly_is_let_go_of_once_another_waits_for_its_turn() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let env = [
        ("ALT_SERVER_WORKERS", "2"),
        ("ALT_SERVER_MAX_FETCH_MS", "120000"),
    ];
    let (server, head) = served(&tmp.path().join("origin"), &env);
    let whole = answer(asked_for(&server.addr, &head, Some("203.0.113.9"))).len();
    let slow = kept(&server, &head, "203.0.113.9");
    let after = waiting(&server, &head, "203.0.113.10");
    let (read, took) = read_slowly(slow);
    assert!(read < whole, "{read} of {whole}");
    assert!(took < Duration::from_secs(90), "{took:?}");
    let after = after.join().unwrap();
    assert!(
        after.starts_with(b"HTTP/1.1 200"),
        "{:.200}",
        String::from_utf8_lossy(&after)
    );
    assert_eq!(after.len(), whole);
}

/// As above, with the least a large response another waits for is taken at
/// told as 64 KiB a second: the one read at a quarter of a megabyte a second
/// is taken fast enough, and is read whole while the other waits.
#[test]
fn a_large_response_taken_at_the_pace_the_server_is_told_is_read_whole_while_another_waits() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let env = [
        ("ALT_SERVER_WORKERS", "2"),
        ("ALT_SERVER_MAX_FETCH_MS", "120000"),
        ("ALT_SERVER_LARGE_MIN_BYTES_PER_SEC", "65536"),
    ];
    let (server, head) = served(&tmp.path().join("origin"), &env);
    let whole = answer(asked_for(&server.addr, &head, Some("203.0.113.9"))).len();
    let slow = kept(&server, &head, "203.0.113.9");
    // it waits for as long as the other is read, which is longer than a fetch waits here
    let (address, head) = (server.addr.clone(), head.clone());
    let after = std::thread::spawn(move || {
        let socket = asked_for(&address, &head, Some("203.0.113.10"));
        let longer = Some(Duration::from_secs(300));
        socket.set_read_timeout(longer).unwrap();
        answer(socket)
    });
    let (read, took) = read_slowly(slow);
    assert_eq!(read, whole, "after {took:?}");
    assert!(took > LARGE_WINDOW, "{took:?}");
    let after = after.join().unwrap();
    assert!(after.starts_with(b"HTTP/1.1 200"));
    assert_eq!(after.len(), whole);
}
