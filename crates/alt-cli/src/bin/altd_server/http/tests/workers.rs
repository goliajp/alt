//! How long a peer keeps a worker: while the body comes, and no longer
//! than a body of that size is given.

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;

/// The time a request waits for the worker is none of its peer's: its
/// body, sent at once, is read when the worker comes to it.
#[test]
fn a_request_that_waited_for_the_worker_has_the_time_of_its_body_from_then() {
    let (release, held) = mpsc::channel();
    let address = told(within(SHORT), Told { held: Some(held) });
    let (mut busy, mut busy_reader) = connected(address);
    busy.write_all(b"GET /hold HTTP/1.1\r\n\r\n").unwrap();
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

#[test]
fn a_body_that_stalls_or_falls_behind_keeps_the_worker_no_longer_than_its_size_is_given() {
    for trickle in [false, true] {
        let address = echo(SHORT);
        let began = Instant::now();
        let (mut slow, mut reader) = connected(address);
        slow.write_all(b"POST /slow HTTP/1.1\r\nContent-Length: 1000000\r\n\r\n")
            .unwrap();
        let sender = std::thread::spawn(move || {
            // two thousand bytes a second, for twenty seconds: far from
            // stalled, and far behind the MiB a second it is given
            let until = Instant::now() + Duration::from_secs(20);
            while trickle && Instant::now() < until && slow.write_all(&[b'x'; 100]).is_ok() {
                std::thread::sleep(Duration::from_millis(50));
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
        // and the worker goes on to the next request
        answered(address);
        drop(reader);
        drop(sender.join().unwrap());
    }
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
