//! What the bodies received take is counted by the address they come from
//! and as it comes on the wire, and a body moves by what it has.

use super::*;
use std::net::{IpAddr, SocketAddr, UdpSocket};

/// An address of this host that is not the loopback one: a peer that
/// connects to it is seen coming from it.
fn outward() -> IpAddr {
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    // nothing is sent: connecting only has the route chosen
    socket.connect("192.0.2.1:9").unwrap();
    let address = socket.local_addr().unwrap().ip();
    assert!(!address.is_loopback() && !address.is_unspecified());
    address
}

/// A server at every address of this host with one worker, which answers a
/// request once its body is received: 200, or 503 where it had no room.
fn receiving(limits: Limits) -> u16 {
    let server = Server::listen("0.0.0.0:0", limits, 16).unwrap();
    let port = server.server_addr().port();
    std::thread::spawn(move || {
        while let Ok(mut request) = server.recv() {
            if !request.received() {
                request.receive(BODY);
                continue;
            }
            let read = request.body(BODY).read_to_end(&mut Vec::new());
            let status = match read.map_err(|error| error.kind()) {
                Ok(_) => 200,
                Err(std::io::ErrorKind::StorageFull) => 503,
                Err(_) => 408,
            };
            request.respond(Response::from_string("").with_status_code(status));
        }
    });
    port
}

/// A request to `address` that says its body has `said` bytes and sends `sent`.
fn posted(address: SocketAddr, said: usize, sent: usize) -> (TcpStream, BufReader<TcpStream>) {
    let (mut stream, reader) = connected(address);
    let head = format!("POST /p HTTP/1.1\r\nContent-Length: {said}\r\n\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(&vec![b'x'; sent]).unwrap();
    (stream, reader)
}

fn status(reader: &mut BufReader<TcpStream>) -> String {
    response(reader).0
}

/// Of four thousand bytes an address has a thousand. One address holds
/// eight hundred in a body that does not end: a second body of its own has
/// no room, and one from another address has. With the first let go of,
/// the address has its room again.
#[test]
fn the_bodies_of_one_address_take_a_quarter_of_the_room_and_leave_the_rest() {
    let limits = Limits {
        spool: 4000,
        each: 8,
        ..within(LONG)
    };
    let port = receiving(limits);
    let (far, near) = (
        SocketAddr::new(outward(), port),
        SocketAddr::from(([127, 0, 0, 1], port)),
    );
    let (mut slow, mut slow_reader) = posted(far, 900, 800);
    // the body of the first is being received before the second comes
    std::thread::sleep(Duration::from_millis(300));
    let (_second, mut reader) = posted(far, 400, 400);
    assert_eq!(status(&mut reader), "HTTP/1.1 503 Service Unavailable");
    let (_other, mut reader) = posted(near, 2000, 2000);
    assert_eq!(status(&mut reader), "HTTP/1.1 200 OK");
    slow.write_all(&[b'x'; 100]).unwrap();
    assert_eq!(status(&mut slow_reader), "HTTP/1.1 200 OK");
    let (_third, mut reader) = posted(far, 400, 400);
    assert_eq!(status(&mut reader), "HTTP/1.1 200 OK");
}

/// Six chunks of one byte, each with five hundred bytes of extension, are
/// three thousand bytes of the four thousand there are: a body of two
/// thousand beside them has no room.
#[test]
fn the_lines_around_the_chunks_take_of_the_room_as_the_chunks_do() {
    let limits = Limits {
        spool: 4000,
        ..within(LONG)
    };
    let near = SocketAddr::from(([127, 0, 0, 1], receiving(limits)));
    let (mut chunked, _chunked_reader) = connected(near);
    let head = b"POST /c HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
    chunked.write_all(head).unwrap();
    let extended = format!("1;x={}\r\ny\r\n", "e".repeat(500 - 9));
    assert_eq!(extended.len(), 500);
    chunked.write_all(extended.repeat(6).as_bytes()).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let (_other, mut reader) = posted(near, 2000, 2000);
    assert_eq!(status(&mut reader), "HTTP/1.1 503 Service Unavailable");
}

/// A body is to move a thousand bytes in each window. Chunks of one byte
/// under four thousand bytes of extension, one every few milliseconds, are
/// far more than that on the wire and far less of the body: the peer is let
/// go of while it still sends, and is not answered with what it sent.
#[test]
fn a_body_moves_by_what_it_has_and_not_by_the_lines_around_its_chunks() {
    let near = SocketAddr::from(([127, 0, 0, 1], receiving(within(SHORT))));
    let (mut chunked, mut reader) = connected(near);
    let head = b"POST /c HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
    chunked.write_all(head).unwrap();
    let extended = format!("1;x={}\r\ny\r\n", "e".repeat(4000));
    let began = Instant::now();
    while began.elapsed() < 5 * SHORT {
        if chunked.write_all(extended.as_bytes()).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = chunked.write_all(b"0\r\n\r\n");
    let status = status(&mut reader);
    assert!(status.starts_with("HTTP/1.1 408"), "{status}");
}
