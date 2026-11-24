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

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn an_address_a_proxy_names_has_so_many_requests_under_way_and_no_more() {
    let mut f = Fixture::new();
    let server = start(&mut f, |_, options| options.connections_each = 2);
    // two requests of one address whose bodies do not come
    let head = "POST /web/login HTTP/1.1\r\nHost: alt\r\nX-Forwarded-For: 203.0.113.9\r\nOrigin: https://workbench.example.test\r\nContent-Type: application/json\r\nContent-Length: 30\r\n\r\n";
    let under_way: Vec<_> = (0..2)
        .map(|_| {
            let (mut socket, reader) = connect(&server);
            socket.write_all(head.as_bytes()).unwrap();
            (socket, reader)
        })
        .collect();
    std::thread::sleep(Duration::from_millis(300));
    let crowded = Some("HTTP/1.1 429 Too Many Requests".to_owned());
    assert_eq!(alive(&server, Some("203.0.113.9")), crowded);
    // and is told when to come again
    let told = answered(&server, Some("203.0.113.9"))
        .unwrap()
        .to_ascii_lowercase();
    assert!(told.contains("\r\nretry-after: 1\r\n"), "{told}");
    let fine = Some("HTTP/1.1 200 OK".to_owned());
    assert_eq!(alive(&server, Some("203.0.113.10")), fine);
    // what the proxy names nobody for is the proxy's own, and has no limit
    assert_eq!(alive(&server, None), fine);
    drop(under_way);
    let until = std::time::Instant::now() + Duration::from_secs(10);
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

#[test]
#[ignore = "requires isolated PostgreSQL and an address of this host beside its loopback; exercised by server acceptance and closing gate"]
fn a_peer_that_is_no_proxy_and_not_this_host_has_so_many_connections_and_no_more() {
    let f = Fixture::new();
    let (factory, mut options) = Running::parts(&f, None, vec![]);
    options.connections_each = 2;
    let server = Running::serve_at(outward(), factory, options, None, false);
    let fine = Some("HTTP/1.1 200 OK".to_owned());
    assert_eq!(alive(&server, None), fine);
    // two connections that say nothing are two all the same
    let silent: Vec<_> = (0..2).map(|_| connect(&server)).collect();
    std::thread::sleep(Duration::from_millis(300));
    // the third is closed unheard
    assert_eq!(alive(&server, None), None);
    drop(silent);
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while alive(&server, None) != fine {
        assert!(std::time::Instant::now() < until, "no place is given back");
        std::thread::sleep(Duration::from_millis(50));
    }
}
