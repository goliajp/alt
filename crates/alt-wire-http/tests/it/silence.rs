//! What the transport gives up on: a connection that goes silent, not an
//! answer that takes long to come in full.

use std::io::Write;
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use alt_wire_http::{GitTransport, Service};

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
    let got = transport.command(Service::UploadPack, b"0000").unwrap();
    assert_eq!(got.len(), whole.len());
    assert!(got == whole);
}

/// A server that sends its status line and then nothing is given up on
#[test]
fn a_connection_silent_for_longer_is_given_up_on() {
    let url = slow_server(Duration::ZERO, vec![b"0008abc\n".to_vec()], SILENCE * 4);
    let transport = GitTransport::new(url).with_timeout(SILENCE);
    assert!(transport.command(Service::UploadPack, b"0000").is_err());
}
