//! Responses to fetches larger than the part of an address: one of an address
//! at a time, as many as the room is told at once, and the others waiting for
//! their turn until their time is over. One kept by a peer that takes it slowly
//! is let go of once another waits for its turn.
use super::sending::{FILE, ROOM, asked, fetched, fetched_at, pushed};
use super::*;
use crate::database::sign_in_deadline::start;
use alt_server::shares::Shares;
use std::io::{BufRead, BufReader, Read};
use std::net::TcpStream;
use std::thread::JoinHandle;
use std::time::Instant;

/// a fetch from `from` sent on a thread of its own: its status and the bytes of its body
fn waiting(
    f: &Fixture,
    server: &Running,
    head: &str,
    from: &'static str,
) -> JoinHandle<(u16, usize)> {
    let (url, token, head) = (server.url.clone(), f.token.clone(), head.to_owned());
    std::thread::spawn(move || {
        let (status, _, bytes) = fetched_at(&url, &token, &head, from);
        (status, bytes)
    })
}

fn until_waiting(room: &Shares, count: usize) {
    let by = Instant::now() + Duration::from_secs(60);
    while room.waiting() != count {
        assert!(Instant::now() < by, "{} wait, not {count}", room.waiting());
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The status of a response that is not read further.
fn begun(reader: &mut BufReader<TcpStream>) -> u16 {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line.split(' ').nth(1).unwrap().parse().unwrap();
    while line != "\r\n" {
        line.clear();
        reader.read_line(&mut line).unwrap();
    }
    status
}

/// The body that is left, read `piece` bytes at a time with `pause` between:
/// how much of it came before it ended.
fn taken(mut reader: BufReader<TcpStream>, piece: usize, pause: Duration) -> usize {
    let (mut all, mut buffer) = (0, vec![0; piece]);
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => return all,
            Ok(read) => all += read,
        }
        std::thread::sleep(pause);
    }
}

/// Two large ones at once, eight megabytes a second in each second for one
/// another waits for. A second of one address waits while the first is there
/// though there is a turn free, and another address has it meanwhile; one
/// that waits while both turns are kept is told to come again when its time is
/// over and not before. One kept and read at a megabyte a second is let go of
/// once another waits, and one read at four megabytes a second is read whole
/// where nobody does.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_large_response_waits_for_its_turn_and_one_kept_slowly_is_let_go_of_for_another() {
    let mut f = Fixture::new();
    let room = Shares::passed_by_one_alone(ROOM, 2).paced(8 << 20, Duration::from_secs(1));
    let server = start(&mut f, |_, options| {
        options.sending = room.clone();
        options.work_timeout = Duration::from_secs(6);
        options.body_timeout = Duration::from_secs(60);
    });
    let head = pushed(&f, &server);
    let (one, other, third) = ("203.0.113.9", "203.0.113.10", "203.0.113.11");
    let (_, _, whole) = fetched(&f, &server, &head, one);
    assert!(whole >= 2 * FILE, "{whole}");

    let mut kept = asked(&f, &server, &head, one);
    assert_eq!(begun(&mut kept), 200);
    let second = waiting(&f, &server, &head, one);
    until_waiting(&room, 1);
    assert_eq!(fetched(&f, &server, &head, other).0, 200);
    std::thread::sleep(Duration::from_secs(1));
    assert!(!second.is_finished());
    drop(kept);
    assert_eq!(second.join().unwrap(), (200, whole));

    let mut kept = asked(&f, &server, &head, one);
    let mut beside = asked(&f, &server, &head, other);
    assert_eq!((begun(&mut kept), begun(&mut beside)), (200, 200));
    let began = Instant::now();
    let (status, _, _) = fetched(&f, &server, &head, third);
    assert_eq!(status, 503);
    assert!(
        began.elapsed() >= Duration::from_secs(5),
        "{:?}",
        began.elapsed()
    );
    until_waiting(&room, 0);

    let after = waiting(&f, &server, &head, third);
    until_waiting(&room, 1);
    let read = taken(kept, 64 << 10, Duration::from_millis(60));
    assert!(read < whole, "{read} of {whole}");
    assert_eq!(after.join().unwrap(), (200, whole));
    drop(beside);
    let mut alone = asked(&f, &server, &head, one);
    assert_eq!(begun(&mut alone), 200);
    assert_eq!(taken(alone, 256 << 10, Duration::from_millis(60)), whole);
}
