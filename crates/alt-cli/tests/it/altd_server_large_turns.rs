//! Responses to fetches larger than the part of an address of the room: one of
//! an address at a time, half as many as there are workers at once, and the
//! others waiting for their turn for as long as a fetch may take. One kept by a
//! peer that takes it slowly is let go of once another waits for its turn.

use std::io::{Read, Write};
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

/// A server of [`BYTES`] of noise, with `env`, and its head: no answer is begun ahead of its pack.
fn served(origin: &std::path::Path, env: &[(&str, &str)]) -> (Server, String) {
    repository(origin);
    std::fs::write(origin.join("noise"), noise(BYTES)).unwrap();
    let alt = crate::altd_server::alt;
    crate::altd_server::ok(alt(origin, &["add", "."]));
    crate::altd_server::ok(alt(origin, &["commit", "-m", "noise"]));
    let mut told = vec![
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_MAX_SENDING_BYTES", "60000000"),
        ("ALT_SERVER_KEEPALIVE_MS", "3600000"),
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
    let late = String::from_utf8_lossy(&late);
    let again = late.starts_with("HTTP/1.1 503") && late.ends_with("fetch again");
    assert!(again, "{late:.300}");
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

/// What is answered to a request for the refs, read for no longer than a
/// third of the window: a kept response nobody reads is let go of at the end
/// of the window, and the turn with it.
fn refs_asked(address: &str) -> Vec<u8> {
    let mut socket = TcpStream::connect(address).unwrap();
    socket.set_read_timeout(Some(LARGE_WINDOW / 3)).unwrap();
    let asked = "GET /info/refs?service=git-upload-pack HTTP/1.1\r\nHost: h\r\nGit-Protocol: version=2\r\nConnection: close\r\n\r\n";
    socket.write_all(asked.as_bytes()).unwrap();
    answer(socket)
}

/// One worker, so one turn. A fetch of another address that waits for the
/// turn while it is kept gives the worker back: a request for the refs that
/// comes after it is answered meanwhile. Once the turn is given back it is
/// answered whole, and the worker it took again serves the next request.
#[test]
fn a_large_response_that_waits_for_its_turn_holds_no_worker() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let env = [
        ("ALT_SERVER_WORKERS", "1"),
        ("ALT_SERVER_MAX_FETCH_MS", "120000"),
    ];
    let (server, head) = served(&tmp.path().join("origin"), &env);
    let whole = answer(asked_for(&server.addr, &head, Some("203.0.113.9"))).len();
    let first = kept(&server, &head, "203.0.113.9");
    let second = waiting(&server, &head, "203.0.113.10");
    // its body is received and it is ahead of the next request
    std::thread::sleep(Duration::from_secs(2));
    let refs = refs_asked(&server.addr);
    assert!(
        refs.starts_with(b"HTTP/1.1 200"),
        "{:.200}",
        String::from_utf8_lossy(&refs)
    );
    assert!(!second.is_finished());
    drop(first);
    let second = second.join().unwrap();
    assert!(
        second.starts_with(b"HTTP/1.1 200"),
        "{:.200}",
        String::from_utf8_lossy(&second)
    );
    assert_eq!(second.len(), whole);
    assert!(refs_asked(&server.addr).starts_with(b"HTTP/1.1 200"));
}

/// The files under `dir`, at any depth, but the bodies of requests: what the
/// server builds responses of under the directory of its own temporary files
/// there. One removed while it is looked at is not.
fn built_of(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in read.flatten() {
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => found.extend(built_of(&entry.path())),
            Ok(_) if entry.file_name() != "body" => found.push(entry.path()),
            _ => {}
        }
    }
    found
}

/// One worker, so one turn. Two fetches of other addresses that would wait
/// for it while it is kept hold nothing they were built of while they wait:
/// what they wrote under the temporary directory of the server is gone, and
/// only their requests are there. Each is built again in its turn and answered whole.
#[test]
fn large_responses_that_wait_for_their_turn_hold_nothing_they_were_built_of() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let built = tmp.path().join("tmp");
    std::fs::create_dir(&built).unwrap();
    let env = [
        ("ALT_SERVER_WORKERS", "1"),
        ("ALT_SERVER_MAX_FETCH_MS", "120000"),
        ("TMPDIR", built.to_str().unwrap()),
    ];
    let (server, head) = served(&tmp.path().join("origin"), &env);
    let whole = answer(asked_for(&server.addr, &head, Some("203.0.113.9"))).len();
    let first = kept(&server, &head, "203.0.113.9");
    let before = built_of(&built);
    let waiters = ["203.0.113.10", "203.0.113.11"].map(|from| waiting(&server, &head, from));
    // their packs are written by then, and found large
    std::thread::sleep(Duration::from_secs(3));
    let by = Instant::now() + Duration::from_secs(30);
    while built_of(&built) != before {
        assert!(
            Instant::now() < by,
            "{:?} beside {before:?}",
            built_of(&built)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(waiters.iter().all(|waiter| !waiter.is_finished()));
    drop(first);
    for waiter in waiters {
        let answered = waiter.join().unwrap();
        assert!(
            answered.starts_with(b"HTTP/1.1 200"),
            "{:.200}",
            String::from_utf8_lossy(&answered)
        );
        assert_eq!(answered.len(), whole);
    }
}
