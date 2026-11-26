//! A response to a fetch that would wait for its turn to be large holds
//! nothing it was built of while it waits: what waits are requests, not the
//! objects they chose or what was written of them, and those being built are
//! no more than the workers. It is built again once it has its turn.
use super::large_turns::{begun, until_waiting, waiting};
use super::sending::{FILE, ROOM, asked, fetched, left, pushed};
use super::*;
use crate::database::git::{identity, ok, text};
use crate::database::sign_in_deadline::start;
use alt_server::shares::Shares;
use std::io::{BufRead, Read, Write};
use std::sync::Arc;

/// a server of one worker and one turn, with the room `room`
fn one_turn(f: &mut Fixture, room: &Arc<Shares>) -> Running {
    start(f, |_, options| {
        options.sending = room.clone();
        options.workers = 1;
        options.work_timeout = Duration::from_secs(120);
        options.body_timeout = Duration::from_secs(120);
    })
}

/// A response kept in the turn, and three of other addresses that chose their
/// objects, found they would be large and wait: none of them is being built,
/// and they take nothing of the room. Each is built again in its turn, whole.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn large_responses_that_wait_for_their_turn_hold_nothing_they_were_built_of() {
    let mut f = Fixture::new();
    let room = Shares::passed_by_one_alone(ROOM, 1);
    let server = one_turn(&mut f, &room);
    let head = pushed(&f, &server);
    let (status, _, whole) = fetched(&f, &server, &head, "203.0.113.9");
    assert!(status == 200 && whole >= 2 * FILE, "{status} {whole}");
    assert_eq!(left(&room), 0);

    let mut kept = asked(&f, &server, &head, "203.0.113.9");
    assert_eq!(begun(&mut kept), 200);
    let held = room.held();
    // one after the other: one that comes while the one worker is taken is
    // told the instance is busy
    let mut waiters = Vec::new();
    for from in ["203.0.113.10", "203.0.113.11", "203.0.113.12"] {
        waiters.push(waiting(&f, &server, &head, from));
        until_waiting(&room, waiters.len());
    }
    assert_eq!((room.building(), room.held()), (0, held));
    drop(kept);
    for waiter in waiters {
        assert_eq!(waiter.join().unwrap(), (200, whole));
    }
    assert_eq!((room.building(), left(&room)), (0, 0));
}

/// A fetch of `want` from `from` that says it has `have`, without a thin pack:
/// its status and the bytes of its body.
fn fetched_having(url: &str, token: &str, want: &str, have: &str, from: &str) -> (u16, usize) {
    let mut command = Vec::new();
    alt_wire::pkt::write_data(&mut command, b"command=fetch\n").unwrap();
    alt_wire::pkt::write_delim(&mut command).unwrap();
    for line in [
        format!("want {want}\n"),
        format!("have {have}\n"),
        "done\n".into(),
    ] {
        alt_wire::pkt::write_data(&mut command, line.as_bytes()).unwrap();
    }
    alt_wire::pkt::write_flush(&mut command).unwrap();
    let head = format!(
        "POST /git/repository/git-upload-pack HTTP/1.1\r\nHost: alt\r\nAuthorization: Bearer {token}\r\nX-Forwarded-For: {from}\r\nGit-Protocol: version=2\r\nContent-Type: application/x-git-upload-pack-request\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        command.len()
    );
    let mut socket = std::net::TcpStream::connect(url.strip_prefix("http://").unwrap()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(120)))
        .unwrap();
    socket.write_all(head.as_bytes()).unwrap();
    socket.write_all(&command).unwrap();
    let mut reader = std::io::BufReader::new(socket);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line.split(' ').nth(1).unwrap().parse().unwrap();
    while line != "\r\n" {
        line.clear();
        reader.read_line(&mut line).unwrap();
    }
    let mut body = Vec::new();
    reader.read_to_end(&mut body).unwrap();
    (status, body.len())
}

/// A file of nine megabytes and the same with a byte changed are stored as
/// the second a delta of the first. A fetch of the second that has the first
/// and takes no thin pack has it written whole: it is found large only as it
/// is written, past what its objects take as they are stored, and is put off
/// then, holding nothing of it while it waits.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_response_found_large_only_as_it_is_written_is_put_off_and_holds_nothing_while_it_waits() {
    let mut f = Fixture::new();
    // a part of eight megabytes: the response kept in the turn and the one
    // that waits both pass it, and the room has them both
    let room = Shares::passed_by_one_alone(32 << 20, 1);
    let server = one_turn(&mut f, &room);
    let head = pushed(&f, &server);
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path();
    let pushed_to = format!("{}/git/repository", server.url);
    ok(work, &f.token, &["init", "-q", "--initial-branch=versions"]);
    identity(work, &f.token);
    // past what a response keeps in memory, and within what an object may be
    let mut data = super::slow_readers::noise(7, 9 << 20);
    std::fs::write(work.join("file"), &data).unwrap();
    ok(work, &f.token, &["add", "."]);
    ok(work, &f.token, &["commit", "-qm", "first"]);
    let first = text(ok(work, &f.token, &["rev-parse", "HEAD"]));
    ok(work, &f.token, &["push", "-q", &pushed_to, "versions"]);
    data[4 << 20] ^= 1;
    std::fs::write(work.join("file"), &data).unwrap();
    ok(work, &f.token, &["commit", "-qam", "second"]);
    let second = text(ok(work, &f.token, &["rev-parse", "HEAD"]));
    ok(work, &f.token, &["push", "-q", &pushed_to, "versions"]);
    let token = f.token.clone();
    let (status, whole) = fetched_having(&server.url, &token, &second, &first, "203.0.113.10");
    assert!(status == 200 && whole > 9 << 20, "{status} {whole}");
    assert_eq!(left(&room), 0);

    let mut kept = asked(&f, &server, &head, "203.0.113.9");
    assert_eq!(begun(&mut kept), 200);
    let held = room.held();
    let (url, want, have) = (server.url.clone(), second.clone(), first.clone());
    let waiter =
        std::thread::spawn(move || fetched_having(&url, &token, &want, &have, "203.0.113.10"));
    until_waiting(&room, 1);
    assert_eq!((room.building(), room.held()), (0, held));
    drop(kept);
    assert_eq!(waiter.join().unwrap(), (200, whole));
    assert_eq!((room.building(), left(&room)), (0, 0));
}
