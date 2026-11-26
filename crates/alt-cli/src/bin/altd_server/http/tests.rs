//! The server against connections made by hand.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;

const GRACE: Duration = Duration::from_secs(20);
const LINGER: Duration = Duration::from_secs(2);

mod workers;

/// the bytes on the wire a body may have for the one worker
const BODY: u64 = 1 << 20;

/// What the one worker does with the requests of some targets.
#[derive(Default)]
struct Told {
    /// `/hold` is not answered before this is sent to or dropped
    held: Option<mpsc::Receiver<()>>,
    /// told when the worker has taken `/hold` up
    taken: Option<mpsc::Sender<()>>,
}

/// A server whose peers are given `limits`, with one worker. It answers
/// a request with its method, its target and its body, which it has
/// received first; one for `/refuse` without its body, one for `/large`
/// with 64 MiB.
fn told(limits: Limits, told: Told) -> std::net::SocketAddr {
    let server = Server::listen("127.0.0.1:0", limits, (4, 1), &LOOPBACK, &LOOPBACK).unwrap();
    let address = server.server_addr();
    std::thread::spawn(move || {
        while let Ok(mut request) = server.recv() {
            match request.url() {
                "/refuse" => {
                    request.respond(Response::from_string("no").with_status_code(401));
                    continue;
                }
                "/large" => {
                    request.respond(Response::from_data(vec![0u8; 64 << 20]));
                    continue;
                }
                "/hold" => {
                    if let Some(taken) = &told.taken {
                        let _ = taken.send(());
                    }
                    let _ = told.held.as_ref().map(mpsc::Receiver::recv);
                }
                _ => {}
            }
            if !request.received() {
                request.receive(BODY);
                continue;
            }
            let mut body = Vec::new();
            let answer = match request.body(BODY).read_to_end(&mut body) {
                Ok(_) => {
                    let said = format!("{:?} {} ", request.method(), request.url());
                    Response::from_data([said.as_bytes(), &body].concat())
                }
                Err(error) => {
                    let status = match error.kind() {
                        std::io::ErrorKind::FileTooLarge => 413,
                        std::io::ErrorKind::StorageFull => 503,
                        _ => 408,
                    };
                    Response::from_string(format!("{:?}", error.kind())).with_status_code(status)
                }
            };
            request.respond(answer);
        }
    });
    address
}

/// A head has `grace`; a body and a response are to move a thousand
/// bytes in each `grace` that passes.
fn within(grace: Duration) -> Limits {
    Limits {
        grace,
        window: grace,
        floor: 1000,
        linger: LINGER.min(grace),
        each: 2,
        spool: 4 * BODY,
    }
}

fn echo(grace: Duration) -> std::net::SocketAddr {
    told(within(grace), Told::default())
}

/// The status and the body of the next response on `reader`.
fn response(reader: &mut BufReader<TcpStream>) -> (String, Vec<u8>, bool) {
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    let (mut length, mut last) = (0, false);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end().to_ascii_lowercase();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("content-length: ") {
            length = value.parse().unwrap();
        }
        last |= line == "connection: close";
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    (status.trim_end().to_owned(), body, last)
}

/// What a read of a connection the server is to have ended gives: nothing, where
/// it has. A read a signal broke into is made again.
fn ended(reader: &mut BufReader<TcpStream>) -> std::io::Result<usize> {
    loop {
        match reader.read(&mut [0]) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            read => return read,
        }
    }
}

fn connected(address: std::net::SocketAddr) -> (TcpStream, BufReader<TcpStream>) {
    let stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let reader = BufReader::new(stream.try_clone().unwrap());
    (stream, reader)
}

#[test]
fn requests_follow_one_another_on_a_connection_with_bodies_of_either_framing() {
    let (mut stream, mut reader) = connected(echo(GRACE));
    stream
        .write_all(b"GET /a?b HTTP/1.1\r\nHost: h\r\n\r\n")
        .unwrap();
    assert_eq!(
        response(&mut reader),
        ("HTTP/1.1 200 OK".into(), b"Get /a?b ".to_vec(), false)
    );
    // the head and the body in pieces, and the next request right behind
    stream.write_all(b"POST /p HTTP/1.1\r\nContent-Le").unwrap();
    stream.write_all(b"ngth: 5\r\n\r\nhel").unwrap();
    stream
        .write_all(b"loPOST /c HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n")
        .unwrap();
    assert_eq!(response(&mut reader).1, b"Post /p hello");
    stream
        .write_all(b"3;x=y\r\nabc\r\n10\r\n0123456789abcdef\r\n0\r\nTrailer: t\r\n\r\n")
        .unwrap();
    assert_eq!(response(&mut reader).1, b"Post /c abc0123456789abcdef");
    // told to go on before it sends the body it announced
    stream
        .write_all(b"POST /e HTTP/1.1\r\nContent-Length: 2\r\nExpect: 100-continue\r\n\r\n")
        .unwrap();
    let mut go_on = [0u8; 25];
    reader.read_exact(&mut go_on).unwrap();
    assert_eq!(&go_on, b"HTTP/1.1 100 Continue\r\n\r\n");
    stream
        .write_all(b"okHEAD /h HTTP/1.1\r\nConnection: close\r\n\r\n")
        .unwrap();
    assert_eq!(response(&mut reader).1, b"Post /e ok");
    let mut rest = String::new();
    reader.read_to_string(&mut rest).unwrap();
    assert!(rest.starts_with("HTTP/1.1 200 OK\r\n"), "{rest}");
    assert!(
        rest.ends_with("Content-Length: 8\r\nConnection: close\r\n\r\n"),
        "{rest}"
    );
}

#[test]
fn a_head_that_is_no_request_is_refused_and_ends_the_connection() {
    for head in [
        "GET / HTTP/1.1\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n".to_owned(),
        "NOT A REQUEST\r\n\r\n".to_owned(),
        format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "x".repeat(70_000)),
    ] {
        let (mut stream, mut reader) = connected(echo(GRACE));
        // the server may end the connection before all of a long head is written
        let _ = stream.write_all(head.as_bytes());
        let (status, _, last) = response(&mut reader);
        assert_eq!(status, "HTTP/1.1 400 Bad Request");
        assert!(last);
        assert_eq!(ended(&mut reader).unwrap_or(0), 0);
    }
}

const SHORT: Duration = Duration::from_millis(300);
/// longer than a test waits for anything
const LONG: Duration = Duration::from_secs(3600);

/// Has the one worker answer a request made now.
fn answered(address: std::net::SocketAddr) {
    let (mut stream, mut reader) = connected(address);
    stream.write_all(b"GET /now HTTP/1.1\r\n\r\n").unwrap();
    assert_eq!(response(&mut reader).1, b"Get /now ");
}

#[test]
fn a_connection_that_sends_no_head_keeps_no_worker_and_is_let_go_of() {
    // however long they are given, the worker is free while the two wait
    let address = echo(LONG);
    let silent = connected(address);
    let (mut partial, _reader) = connected(address);
    partial.write_all(b"GET /never HTTP/1.1\r\nHost").unwrap();
    answered(address);
    drop(silent);

    let address = echo(SHORT);
    let began = Instant::now();
    let (mut silent, mut silent_reader) = connected(address);
    let (mut partial, mut partial_reader) = connected(address);
    partial.write_all(b"GET /never HTTP/1.1\r\nHost").unwrap();
    // the one that sent nothing is ended, the one that began is told why
    assert_eq!(ended(&mut silent_reader).unwrap(), 0);
    let (status, _, last) = response(&mut partial_reader);
    assert_eq!(status, "HTTP/1.1 408 Request Timeout");
    assert!(last);
    assert!(began.elapsed() >= SHORT);
    let _ = silent.write_all(b"x");
}

/// A head that comes a byte at a time has the grace and no more: what
/// came of it buys it nothing.
#[test]
fn a_head_that_trickles_is_given_up_when_the_grace_is_over() {
    let (mut slow, mut reader) = connected(echo(SHORT));
    // the head is begun before the grace can be over: a connection that sent
    // nothing in it is ended without an answer, and a thread may be late
    slow.write_all(b"GET /never HTTP/1.1\r\nX: ").unwrap();
    let sender = std::thread::spawn(move || {
        for byte in [b'x'; 4096] {
            if slow.write_all(&[byte]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        slow
    });
    let began = Instant::now();
    let (status, _, last) = response(&mut reader);
    assert_eq!(
        (status.as_str(), last),
        ("HTTP/1.1 408 Request Timeout", true)
    );
    // the whole of it would have taken eighty seconds
    assert!(began.elapsed() < Duration::from_secs(40));
    drop(sender.join().unwrap());
}

#[test]
fn a_connection_above_the_limit_is_not_taken() {
    let address = echo(GRACE);
    let mut held: Vec<_> = (0..4).map(|_| connected(address)).collect();
    // each of the four is taken: it is answered
    for (stream, reader) in &mut held {
        stream.write_all(b"GET /held HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(response(reader).1, b"Get /held ");
    }
    let (mut fifth, mut reader) = connected(address);
    let _ = fifth.write_all(b"GET /fifth HTTP/1.1\r\n\r\n");
    assert_eq!(ended(&mut reader).unwrap_or(0), 0);
    // one of the four ends, and the next is taken
    drop(held);
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let (mut stream, mut reader) = connected(address);
        let _ = stream.write_all(b"GET /next HTTP/1.1\r\n\r\n");
        let mut status = String::new();
        if reader.read_line(&mut status).is_ok_and(|read| read > 0) {
            break assert_eq!(status, "HTTP/1.1 200 OK\r\n");
        }
        assert!(Instant::now() < until, "no connection is taken again");
        std::thread::sleep(Duration::from_millis(10));
    }
}
