//! The requests one address has under way at once where a proxy names it, and
//! the connections of a peer that is no proxy and not this host as it listens to
//! itself: for that the service is reached at the address this host has towards
//! others.
use super::bodies::connect;
use super::*;
use crate::database::sign_in_deadline::start;
use std::io::{Read, Write};

/// what `GET /livez` is answered with over a connection of its own, as `from`
/// where a proxy is to have said so
fn alive(server: &Running, from: Option<&str>) -> Option<String> {
    answered(server, from)?.lines().next().map(str::to_owned)
}

/// the whole of that answer
fn answered(server: &Running, from: Option<&str>) -> Option<String> {
    let (mut socket, _) = connect(server);
    let named = from.map_or(String::new(), |from| format!("X-Forwarded-For: {from}\r\n"));
    let request = format!("GET /livez HTTP/1.1\r\nHost: alt\r\n{named}Connection: close\r\n\r\n");
    socket.write_all(request.as_bytes()).ok()?;
    let mut answer = String::new();
    socket.read_to_string(&mut answer).ok()?;
    Some(answer)
}

/// The status line of the next response on a connection and its headers in small
/// letters, with its body read and let go of.
fn head_of(reader: &mut std::io::BufReader<std::net::TcpStream>) -> (String, String) {
    use std::io::BufRead;
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line.trim_end().to_owned();
    let mut headers = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line.trim_end().is_empty() {
            break;
        }
        headers.push_str(&line.to_ascii_lowercase());
    }
    let length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"));
    let mut body = vec![0; length.map_or(0, |length| length.trim().parse().unwrap())];
    reader.read_exact(&mut body).unwrap();
    (status, headers)
}

/// Whether a request that was to stay unanswered was answered, or let go of.
/// What the service answered on `socket` already, as far as it came: none
/// where nothing came.
fn answered_already(socket: &std::net::TcpStream) -> Option<String> {
    socket.set_nonblocking(true).unwrap();
    let mut come = [0; 64];
    let read = socket.peek(&mut come);
    socket.set_nonblocking(false).unwrap();
    match read {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
        read => Some(String::from_utf8_lossy(&come[..read.unwrap_or(0)]).into_owned()),
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn an_address_a_proxy_names_has_so_many_requests_under_way_and_no_more() {
    let mut f = Fixture::new();
    // two requests of one address whose bodies do not come, and are waited
    // for longer than the test takes
    let server = start(&mut f, |_, options| {
        options.connections_each = 2;
        options.body_timeout = Duration::from_secs(600);
    });
    let head = "POST /web/login HTTP/1.1\r\nHost: alt\r\nX-Forwarded-For: 203.0.113.9\r\nOrigin: https://workbench.example.test\r\nContent-Type: application/json\r\nContent-Length: 30\r\n\r\n";
    let held = || {
        let (mut socket, reader) = connect(&server);
        socket.write_all(head.as_bytes()).unwrap();
        (socket, reader)
    };
    let mut under_way: Vec<_> = (0..2).map(|_| held()).collect();
    // a third is answered for as long as the two are not both under way, and
    // is itself done with once it is: it is refused from when they are. the
    // service may come to a third before one of the two, which then finds
    // its place taken and is refused: that one is sent again
    let crowded = Some("HTTP/1.1 429 Too Many Requests".to_owned());
    let until = std::time::Instant::now() + Duration::from_secs(120);
    while alive(&server, Some("203.0.113.9")) != crowded {
        assert!(std::time::Instant::now() < until, "a third is answered");
        for one in &mut under_way {
            // refused for its place, and for nothing else
            if let Some(answer) = answered_already(&one.0) {
                assert!(answer.starts_with("HTTP/1.1 429 "), "{answer:?}");
                *one = held();
            }
        }
    }
    // and is told when to come again
    let told = answered(&server, Some("203.0.113.9"))
        .unwrap()
        .to_ascii_lowercase();
    assert!(told.contains("\r\nretry-after: 1\r\n"), "{told}");
    // The refusal of a request without a body leaves the connection, which is
    // the proxy's, to the next request on it; that of one whose body was not
    // read ends it.
    let (mut socket, mut reader) = connect(&server);
    let get =
        |from: &str| format!("GET /livez HTTP/1.1\r\nHost: alt\r\nX-Forwarded-For: {from}\r\n\r\n");
    socket.write_all(get("203.0.113.9").as_bytes()).unwrap();
    let (status, headers) = head_of(&mut reader);
    assert_eq!(status, "HTTP/1.1 429 Too Many Requests");
    assert!(!headers.contains("connection: close"), "{headers}");
    socket.write_all(get("203.0.113.10").as_bytes()).unwrap();
    assert_eq!(head_of(&mut reader).0, "HTTP/1.1 200 OK");
    let post = head.replace("Content-Length: 30", "Content-Length: 3");
    socket.write_all(post.as_bytes()).unwrap();
    let (status, headers) = head_of(&mut reader);
    assert_eq!(status, "HTTP/1.1 429 Too Many Requests");
    assert!(headers.contains("connection: close"), "{headers}");
    let fine = Some("HTTP/1.1 200 OK".to_owned());
    assert_eq!(alive(&server, Some("203.0.113.10")), fine);
    // what the proxy names nobody for is the proxy's own, and has no limit
    assert_eq!(alive(&server, None), fine);
    drop(under_way);
    let until = std::time::Instant::now() + Duration::from_secs(120);
    while alive(&server, Some("203.0.113.9")) != fine {
        assert!(std::time::Instant::now() < until, "no place is given back");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The address this host has towards others: what a peer on it is seen as when
/// it connects to that address.
fn outward() -> std::net::IpAddr {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    // nothing is sent: connecting only has the route chosen
    socket
        .connect("192.0.2.1:9")
        .expect("a route to other hosts");
    let address = socket.local_addr().unwrap().ip();
    assert!(
        !address.is_loopback() && !address.is_unspecified(),
        "{address}"
    );
    address
}

/// Whether a connection of its own is closed with nothing said on it: answered
/// with nothing, and not left to wait for an answer either.
fn closed_unheard(server: &Running) -> bool {
    let (mut socket, _) = connect(server);
    let request = b"GET /livez HTTP/1.1\r\nHost: alt\r\nConnection: close\r\n\r\n";
    let _ = socket.write_all(request);
    let unanswered = [std::io::ErrorKind::WouldBlock, std::io::ErrorKind::TimedOut];
    match socket.read(&mut [0]) {
        Ok(read) => read == 0,
        Err(error) => !unanswered.contains(&error.kind()),
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL and an address of this host beside its loopback; exercised by server acceptance and closing gate"]
fn a_peer_that_is_no_proxy_and_not_this_host_has_so_many_connections_and_no_more() {
    let f = Fixture::new();
    let (factory, mut options) = Running::parts(&f, None, vec![]);
    options.connections_each = 2;
    // the two that say nothing are still there when the third comes, however
    // long the host takes to come to it
    options.header_timeout = Duration::from_secs(120);
    let server = Running::serve_at(outward(), factory, options, None, false);
    let fine = Some("HTTP/1.1 200 OK".to_owned());
    assert_eq!(alive(&server, None), fine);
    // two connections that say nothing are two all the same
    let silent: Vec<_> = (0..2).map(|_| connect(&server)).collect();
    // A third is answered for as long as the server has not taken both, and is
    // gone once it is answered: from when it has taken them, the third is
    // closed unheard, however long the host took to come to them.
    let until = std::time::Instant::now() + Duration::from_secs(120);
    while !closed_unheard(&server) {
        assert!(std::time::Instant::now() < until, "a third is answered");
    }
    drop(silent);
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while alive(&server, None) != fine {
        assert!(std::time::Instant::now() < until, "no place is given back");
        std::thread::sleep(Duration::from_millis(50));
    }
}
