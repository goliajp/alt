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
    receiving_here(limits, &LOOPBACK)
}

/// [`receiving`], with the peers of the networks `here` counted for nothing.
fn receiving_here(limits: Limits, here: &'static [Network]) -> u16 {
    let server = Server::listen("0.0.0.0:0", limits, 16, here).unwrap();
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

/// An address may have one connection, and the outward address of this host is
/// one like any other: its second connection is not taken. A server told that
/// the address is of this host, as the address a container's network gives the
/// proxy in front of it is, takes as many as come from it, and holds what they
/// send to the room alone.
#[test]
fn the_peers_of_a_network_told_to_be_of_this_host_are_held_to_no_part() {
    let limits = Limits {
        spool: 4000,
        each: 1,
        ..within(LONG)
    };
    // whether a second connection from the outward address is answered
    let second = |port: u16| {
        let far = SocketAddr::new(outward(), port);
        let (_first, _reader) = posted(far, 2000, 1900);
        std::thread::sleep(Duration::from_millis(300));
        let mut stream = TcpStream::connect(far).unwrap();
        stream.set_read_timeout(Some(LONG)).unwrap();
        let sent = stream.write_all(b"POST /p HTTP/1.1\r\nContent-Length: 1500\r\n\r\n");
        let sent = sent.and_then(|()| stream.write_all(&[b'x'; 1500]));
        let mut line = String::new();
        let read = sent.and_then(|()| BufReader::new(stream).read_line(&mut line));
        read.ok().filter(|read| *read > 0).map(|_| line)
    };
    assert_eq!(second(receiving(limits)), None);
    let here = networks(&format!("127.0.0.0/8,{}", outward())).unwrap();
    let told = second(receiving_here(limits, Vec::leak(here)));
    // more than a quarter of the room beside the first, which has as much
    assert_eq!(told.as_deref(), Some("HTTP/1.1 200 OK\r\n"));
}

/// The connections of an address are counted for it, those of the
/// network of a host for IPv6, and those of this host for nobody.
#[test]
fn an_address_has_so_many_connections_open_and_no_more() {
    let shared = Arc::new(Shared {
        waiting: Mutex::default(),
        arrived: Condvar::new(),
        open: AtomicUsize::new(0),
        from: Mutex::default(),
        held: Shares::of(u64::MAX),
        limits: within(GRACE),
        here: &LOOPBACK,
    });
    let open = |address: &str| shared.open(address.parse().unwrap(), 8);
    let two = [open("203.0.113.7"), open("::ffff:203.0.113.7")];
    assert!(two.iter().all(Option::is_some));
    assert!(open("203.0.113.7").is_none());
    let other = [
        open("203.0.113.8"),
        open("2001:db8::1"),
        open("2001:db8::2"),
    ];
    assert!(other.iter().all(Option::is_some));
    assert!(open("2001:db8:0:0:1::").is_none());
    assert!(open("2001:db8:0:1::1").is_some());
    assert!(open("fe80::1").is_some() && open("fe80::2").is_some());
    drop(two);
    assert!(open("203.0.113.7").is_some());
    // a proxy on this host stands for everybody behind it
    let local: Vec<_> = (0..4)
        .map(|_| open("127.0.0.1"))
        .chain([open("::1")])
        .collect();
    assert!(local.iter().all(Option::is_some));
    // and all of them together are within what the server takes
    assert_eq!(shared.open.load(Ordering::Acquire), 8);
    assert!(open("127.0.0.1").is_none() && open("198.51.100.1").is_none());
    assert_eq!(shared.open.load(Ordering::Acquire), 8);
    drop((other, local));
    assert_eq!(shared.open.load(Ordering::Acquire), 0);
    assert!(shared.from.lock().unwrap().is_empty());
}
