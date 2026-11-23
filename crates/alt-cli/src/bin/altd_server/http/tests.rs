//! The server against connections made by hand.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use super::*;

/// A server whose peers are given `grace`, with one worker that answers
/// each request with its method, its target and its body.
fn echo(grace: Duration) -> std::net::SocketAddr {
    let server = Server::listen("127.0.0.1:0", grace, 4).unwrap();
    let address = server.server_addr();
    std::thread::spawn(move || {
        while let Ok(mut request) = server.recv() {
            let mut body = Vec::new();
            let answer = match request.as_reader().read_to_end(&mut body) {
                Ok(_) => {
                    let said = format!("{:?} {} ", request.method(), request.url());
                    Response::from_data([said.as_bytes(), &body].concat())
                }
                Err(error) => {
                    Response::from_string(format!("{:?}", error.kind())).with_status_code(408)
                }
            };
            let _ = request.respond(answer);
        }
    });
    address
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
        assert_eq!(reader.read(&mut [0]).unwrap_or(0), 0);
    }
}

const SHORT: Duration = Duration::from_millis(300);

/// How long the one worker takes to answer a request made now.
fn answered(address: std::net::SocketAddr) -> Duration {
    let began = Instant::now();
    let (mut stream, mut reader) = connected(address);
    stream.write_all(b"GET /now HTTP/1.1\r\n\r\n").unwrap();
    assert_eq!(response(&mut reader).1, b"Get /now ");
    began.elapsed()
}

#[test]
fn a_connection_that_sends_no_head_keeps_no_worker_and_is_let_go_of() {
    let address = echo(SHORT);
    let began = Instant::now();
    let (mut silent, mut silent_reader) = connected(address);
    let (mut partial, mut partial_reader) = connected(address);
    partial.write_all(b"GET /never HTTP/1.1\r\nHost").unwrap();
    // the worker is free while the two wait
    assert!(answered(address) < SHORT, "the worker waited for a head");
    // the one that sent nothing is ended, the one that began is told why
    assert_eq!(silent_reader.read(&mut [0]).unwrap(), 0);
    let (status, _, last) = response(&mut partial_reader);
    assert_eq!(status, "HTTP/1.1 408 Request Timeout");
    assert!(last);
    assert!(began.elapsed() >= SHORT);
    let _ = silent.write_all(b"x");
}

#[test]
fn a_body_that_stalls_or_trickles_keeps_the_worker_no_longer_than_its_peer_is_given() {
    for trickle in [false, true] {
        let address = echo(SHORT);
        let began = Instant::now();
        let (mut slow, mut reader) = connected(address);
        slow.write_all(b"POST /slow HTTP/1.1\r\nContent-Length: 1000\r\n\r\n")
            .unwrap();
        let sender = std::thread::spawn(move || {
            // a byte every tenth of a second would take a hundred seconds
            while trickle && slow.write_all(b"x").is_ok() {
                std::thread::sleep(Duration::from_millis(100));
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
    assert_eq!(reader.read(&mut [0]).unwrap_or(0), 0);
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
