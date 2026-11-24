//! A peer keeps no worker: not while its body comes, however slowly, not
//! while it does not take its answer; and one that does not go on is let
//! go of.

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;

mod sources;

/// The time a request waits for the worker is none of its peer's: its
/// body, sent at once, is read when the worker comes to it.
#[test]
fn a_request_that_waited_for_the_worker_has_the_time_of_its_body_from_then() {
    let (release, held) = mpsc::channel();
    let (taken, took) = mpsc::channel();
    let limits = within(SHORT);
    let (held, taken) = (Some(held), Some(taken));
    let address = told(limits, Told { held, taken });
    let (mut busy, mut busy_reader) = connected(address);
    busy.write_all(b"GET /hold HTTP/1.1\r\n\r\n").unwrap();
    // the worker has the one that keeps it before the other is sent
    took.recv_timeout(Duration::from_secs(60)).unwrap();
    let (mut waiting, mut reader) = connected(address);
    let head = b"POST /p HTTP/1.1\r\nContent-Length: 5\r\n\r\n";
    waiting.write_all(head).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    waiting.write_all(b"hello").unwrap();
    // the worker is busy for longer than a peer is given
    std::thread::sleep(3 * SHORT);
    release.send(()).unwrap();
    assert_eq!(response(&mut busy_reader).1, b"Get /hold ");
    let (status, body, _) = response(&mut reader);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(body, b"Post /p hello");
}

/// A body that does not come, and one of which too little comes in the
/// time a body has to move in, are given up, in that time or the next.
#[test]
fn a_body_that_stalls_or_dribbles_is_let_go_of() {
    for dribble in [false, true] {
        let address = echo(SHORT);
        let began = Instant::now();
        let (mut slow, mut reader) = connected(address);
        slow.write_all(b"POST /slow HTTP/1.1\r\nContent-Length: 1000000\r\n\r\n")
            .unwrap();
        let sender = std::thread::spawn(move || {
            // some hundred bytes in the time a thousand are to come in
            let until = Instant::now() + Duration::from_secs(20);
            while dribble && Instant::now() < until && slow.write_all(&[b'x'; 10]).is_ok() {
                std::thread::sleep(Duration::from_millis(30));
            }
            slow
        });
        let (status, body, last) = response(&mut reader);
        assert_eq!(
            (status.as_str(), last),
            ("HTTP/1.1 408 Request Timeout", true)
        );
        assert_eq!(body, b"TimedOut");
        let took = began.elapsed();
        assert!(took >= SHORT && took < Duration::from_secs(10), "{took:?}");
        answered(address);
        drop(reader);
        drop(sender.join().unwrap());
    }
}

/// A body that comes slowly and goes on coming is taken to its end, over
/// many times the time one has to move in, and the worker answers other
/// requests while it comes.
#[test]
fn a_body_that_is_slow_and_goes_on_is_received_and_keeps_no_worker_meanwhile() {
    let address = echo(SHORT);
    let (mut slow, mut reader) = connected(address);
    slow.write_all(b"POST /slow HTTP/1.1\r\nContent-Length: 100000\r\n\r\n")
        .unwrap();
    let began = Instant::now();
    // a thousand bytes each twenty milliseconds: fifteen times what is
    // asked of it, and two seconds for all of it
    for sent in 0..100 {
        slow.write_all(&[b'x'; 1000]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        if sent % 25 == 0 {
            answered(address);
        }
    }
    let (status, body, _) = response(&mut reader);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(body.len(), "Post /slow ".len() + 100_000);
    assert!(began.elapsed() >= 5 * SHORT);
}

/// The bodies received and not yet let go of have a size between them:
/// one that passes it is told that there is no room now.
#[test]
fn a_body_there_is_no_room_for_is_told_so() {
    let limits = Limits {
        spool: 1000,
        ..within(LONG)
    };
    let address = told(limits, Told::default());
    let (mut stream, mut reader) = connected(address);
    stream
        .write_all(b"POST /p HTTP/1.1\r\nContent-Length: 5000\r\n\r\n")
        .unwrap();
    stream.write_all(&[b'x'; 5000]).unwrap();
    let (status, body, _) = response(&mut reader);
    assert_eq!(status, "HTTP/1.1 503 Service Unavailable");
    assert_eq!(body, b"StorageFull");
    // what it took is given back with it
    let (mut stream, mut reader) = connected(address);
    stream
        .write_all(b"POST /p HTTP/1.1\r\nContent-Length: 900\r\n\r\n")
        .unwrap();
    stream.write_all(&[b'y'; 900]).unwrap();
    assert_eq!(response(&mut reader).0, "HTTP/1.1 200 OK");
}

/// The lines of a chunked body are bytes of it: a peer does not keep
/// the worker with trailers without end, nor with chunks that are all
/// extension.
#[test]
fn the_lines_around_the_chunks_count_against_the_bytes_a_body_may_have() {
    let address = echo(SHORT);
    let head = b"POST /c HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
    let trailer = format!("X-T: {}\r\n", "a".repeat(1000));
    let (mut stream, mut reader) = connected(address);
    stream.write_all(head).unwrap();
    stream.write_all(b"1\r\nx\r\n0\r\n").unwrap();
    // four times what the body may have, and no end of them
    for _ in 0..4 * BODY as usize / trailer.len() {
        if stream.write_all(trailer.as_bytes()).is_err() {
            break;
        }
    }
    let (status, body, last) = response(&mut reader);
    assert_eq!(
        (status.as_str(), last),
        ("HTTP/1.1 413 Payload Too Large", true)
    );
    assert_eq!(body, b"FileTooLarge");
    answered(address);

    let (mut stream, mut reader) = connected(address);
    stream.write_all(head).unwrap();
    let extended = format!("1;x={}\r\ny\r\n", "e".repeat(4000));
    for _ in 0..2 * BODY as usize / extended.len() {
        if stream.write_all(extended.as_bytes()).is_err() {
            break;
        }
    }
    let _ = stream.write_all(b"0\r\n\r\n");
    assert_eq!(response(&mut reader).0, "HTTP/1.1 413 Payload Too Large");
    answered(address);
}

/// The worker hands its answer over and is free: a peer that does not
/// read it, however long it is given to, keeps the worker from nothing.
#[test]
fn a_peer_that_does_not_take_its_answer_keeps_no_worker() {
    let address = echo(LONG);
    let (mut idle, _reader) = connected(address);
    idle.write_all(b"GET /large HTTP/1.1\r\n\r\n").unwrap();
    answered(address);
    answered(address);
}

/// A request answered before its body came has its connection ended by
/// the thread of the connection, which takes what the peer still sends
/// for as long as that is done: the worker answers the next request
/// meanwhile.
#[test]
fn the_end_of_a_connection_is_not_waited_for_by_the_worker() {
    let limits = Limits {
        linger: LONG,
        ..within(LONG)
    };
    let address = told(limits, Told::default());
    let (mut unsent, mut reader) = connected(address);
    unsent
        .write_all(b"POST /refuse HTTP/1.1\r\nContent-Length: 100000\r\n\r\n")
        .unwrap();
    let (status, _, last) = response(&mut reader);
    assert_eq!((status.as_str(), last), ("HTTP/1.1 401 Unauthorized", true));
    // nothing more is sent to the peer, and what it sends is still taken
    assert_eq!(reader.read(&mut [0]).unwrap(), 0);
    unsent.write_all(b"some of the body").unwrap();
    answered(address);
    drop(unsent);
}
