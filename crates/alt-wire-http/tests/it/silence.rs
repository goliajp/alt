//! What the transport gives up on: a connection that goes silent, not an
//! answer that takes long to come in full.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use alt_wire_http::{GitTransport, Service, TransportError};

const SILENCE: Duration = Duration::from_secs(1);

/// A server that answers the one request it gets with the status line and
/// headers after `before`, then each of `pieces` after `between`, then
/// closes. Returns the URL to dial.
fn slow_server(before: Duration, pieces: Vec<Vec<u8>>, between: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        super::read_request(&mut stream);
        thread::sleep(before);
        let head = "HTTP/1.1 200 OK\r\n\
                    Content-Type: application/x-git-upload-pack-result\r\n\
                    Connection: close\r\n\r\n";
        // the client may have given up and gone
        if stream.write_all(head.as_bytes()).is_err() {
            return;
        }
        for piece in pieces {
            thread::sleep(between);
            if stream.write_all(&piece).is_err() {
                return;
            }
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// A pack that comes in pieces for three times as long as the connection
/// may stay silent, never silent that long, comes in whole
#[test]
fn an_answer_that_keeps_coming_is_read_however_long_it_takes() {
    let pieces: Vec<Vec<u8>> = (0..30u8).map(|i| vec![i; 1000]).collect();
    let whole = pieces.concat();
    let url = slow_server(SILENCE / 2, pieces, SILENCE / 10);
    let transport = GitTransport::new(url).with_timeout(SILENCE);
    let got = transport
        .command(Service::UploadPack, b"0000", u64::MAX)
        .unwrap();
    assert_eq!(got.len(), whole.len());
    assert!(got == whole);
}

/// A server that sends its status line and then nothing is given up on
#[test]
fn a_connection_silent_for_longer_is_given_up_on() {
    let url = slow_server(Duration::ZERO, vec![b"0008abc\n".to_vec()], SILENCE * 4);
    let transport = GitTransport::new(url).with_timeout(SILENCE);
    assert!(
        transport
            .command(Service::UploadPack, b"0000", u64::MAX)
            .is_err()
    );
}

/// Reads the head of a request off `stream`; false where the peer closed
/// the connection before one came.
fn head_read(stream: &mut TcpStream) -> bool {
    let mut head = Vec::new();
    let mut byte = [0u8];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return false,
        }
    }
    true
}

/// A server that answers the first request it gets and leaves the
/// connection open for another, and answers no other request: it reads
/// the head of each where `reads`, and nothing of it where not. Returns
/// the URL to dial.
fn answers_once(reads: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let mut held = Vec::new();
        let mut answered = false;
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            if !answered {
                if !head_read(&mut stream) {
                    continue;
                }
                let answer = "HTTP/1.1 200 OK\r\n\
                              Content-Type: application/x-git-upload-pack-advertisement\r\n\
                              Content-Length: 4\r\n\r\n0000";
                stream.write_all(answer.as_bytes()).unwrap();
                answered = true;
            }
            // the next request, on this connection or on another
            if reads && !head_read(&mut stream) {
                continue;
            }
            held.push(stream);
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// What `ask` gives back, or a failure where it has not come back in ten
/// times the silence a connection is given.
fn within_bound<T: Send + 'static>(ask: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, came) = mpsc::channel();
    thread::spawn(move || {
        let _ = done.send(ask());
    });
    came.recv_timeout(SILENCE * 10)
        .expect("the request waits without end")
}

/// A second request to a server that does not answer it is given up on
/// once the connection was silent for as long as it may be, as the first
/// would be: the second does not go out on a connection that has no limit
#[test]
fn a_second_request_that_no_status_line_comes_for_is_given_up_on() {
    let url = answers_once(true);
    let got = within_bound(move || {
        let transport = GitTransport::new(url).with_timeout(SILENCE);
        transport.info_refs(Service::UploadPack, u64::MAX).unwrap();
        transport
            .command(Service::UploadPack, b"0000", u64::MAX)
            .map(drop)
    });
    assert!(got.is_err());
}

/// The same for a second request whose body the server does not take
#[test]
fn a_second_request_whose_body_is_not_taken_is_given_up_on() {
    let url = answers_once(false);
    let got = within_bound(move || {
        let transport = GitTransport::new(url).with_timeout(SILENCE);
        transport.info_refs(Service::UploadPack, u64::MAX).unwrap();
        // more than the sockets of a connection hold between them
        let body = vec![b'0'; 64 << 20];
        transport
            .command(Service::UploadPack, &body, u64::MAX)
            .map(drop)
    });
    assert!(got.is_err());
}

/// A server that answers the one request it gets with `head` and then
/// `body`, and closes. Returns the URL to dial.
fn answers(head: &'static str, body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        super::read_request(&mut stream);
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&body);
    });
    format!("http://127.0.0.1:{port}")
}

/// An answer of more bytes than the caller reads into memory is an error,
/// whether it says its length or ends where its connection does
#[test]
fn an_answer_larger_than_its_caller_takes_is_an_error() {
    let sized = "HTTP/1.1 200 OK\r\nContent-Length: 1001\r\nConnection: close\r\n\r\n";
    let to_close = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n";
    for head in [sized, to_close] {
        let transport = GitTransport::new(answers(head, vec![b'0'; 1001]));
        let got = transport.command(Service::UploadPack, b"0000", 1000);
        assert!(
            matches!(got, Err(TransportError::TooLarge { most: 1000 })),
            "{head}: {got:?}"
        );
        let transport = GitTransport::new(answers(head, vec![b'0'; 1001]));
        assert_eq!(
            transport
                .info_refs(Service::UploadPack, 1001)
                .unwrap()
                .len(),
            1001
        );
    }
}

/// A pack is handed over as it comes: the first of it is there to be read
/// while the rest is not sent yet
#[test]
fn a_streamed_answer_is_read_before_it_is_whole() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (read, first_read) = mpsc::channel::<()>();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        super::read_request(&mut stream);
        let head = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nfirst";
        stream.write_all(head.as_bytes()).unwrap();
        // the rest only once the first was read, or never
        if first_read.recv_timeout(SILENCE * 10).is_ok() {
            stream.write_all(b"rest").unwrap();
        }
    });
    let url = format!("http://127.0.0.1:{port}");
    let got = within_bound(move || {
        let transport = GitTransport::new(url).with_timeout(SILENCE * 20);
        let mut answer = transport
            .command_stream(Service::UploadPack, b"0000")
            .unwrap();
        let mut first = [0u8; 5];
        answer.read_exact(&mut first).unwrap();
        read.send(()).unwrap();
        let mut rest = Vec::new();
        answer.read_to_end(&mut rest).unwrap();
        (first, rest)
    });
    assert_eq!(&got.0, b"first");
    assert_eq!(got.1, b"rest");
}
