//! A server that stops answering does not keep the client waiting without
//! end, and one that never stops sending does not fill its memory.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use alt_lfs::{Client, Pointer};

const SILENCE: Duration = Duration::from_secs(1);

/// Reads the head and the body of a request off `stream`; false where the
/// peer closed the connection before one came.
fn request_read(stream: &mut TcpStream) -> bool {
    let mut head = Vec::new();
    let mut byte = [0u8];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read_exact(&mut byte).is_err() {
            return false;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8(head).unwrap().to_ascii_lowercase();
    let length = head.lines().find_map(|l| l.strip_prefix("content-length:"));
    let length: usize = length.map_or(0, |l| l.trim().parse().unwrap());
    stream.read_exact(&mut vec![0u8; length]).is_ok()
}

/// A server that answers the batch request for `pointer` with a download
/// from itself, leaving the connection open for another request, and hands
/// the connection the download comes on to `download`. Returns the endpoint.
fn serve(pointer: &Pointer, download: fn(TcpStream)) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let batch = alt_json::json!({"transfer": "basic", "objects": [{
        "oid": pointer.oid, "size": pointer.size,
        "actions": {"download": {"href": format!("{base}/content")}}}]})
    .to_string();
    thread::spawn(move || {
        let mut answered = false;
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            if !answered {
                if !request_read(&mut stream) {
                    continue;
                }
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", batch.len());
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(batch.as_bytes()).unwrap();
                answered = true;
            }
            // the download, on this connection or on the next
            if request_read(&mut stream) {
                return download(stream);
            }
        }
    });
    format!("{base}/repo.git/info/lfs")
}

/// What `ask` gives back, or a failure where it has not come back in ten
/// times the silence a connection is given.
fn within_bound<T: Send + 'static>(ask: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, came) = mpsc::channel();
    thread::spawn(move || {
        let _ = done.send(ask());
    });
    came.recv_timeout(SILENCE * 10)
        .expect("the download waits without end")
}

#[test]
fn a_download_that_no_answer_comes_for_is_given_up_on() {
    let pointer = Pointer::of(b"weights");
    let endpoint = serve(&pointer, |stream| {
        thread::sleep(SILENCE * 20);
        drop(stream);
    });
    let got = within_bound(move || {
        let client = Client::new(endpoint, None).with_timeout(SILENCE);
        client.download(&[pointer]).map(drop)
    });
    assert!(got.is_err());
}

#[test]
fn content_that_never_ends_is_read_no_further_than_its_size() {
    let pointer = Pointer::of(b"weights");
    let endpoint = serve(&pointer, |mut stream| {
        let head = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n";
        let mut sent = stream.write_all(head.as_bytes());
        while sent.is_ok() {
            sent = stream.write_all(&[b'w'; 64 << 10]);
        }
    });
    let got = within_bound(move || {
        let client = Client::new(endpoint, None).with_timeout(SILENCE);
        client.download(&[pointer]).map(drop)
    });
    assert!(got.is_err());
}
