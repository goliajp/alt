//! The connections one address has in use at once: its own where it is the peer,
//! and its requests under way where a proxy names it.
use super::bodies::connect;
use super::*;
use crate::database::sign_in_deadline::start;
use std::io::{Read, Write};

/// what `GET /livez` is answered with over a connection of its own, as `from`
/// where a proxy is to have said so; nothing where the connection is ended
fn alive(server: &Running, from: Option<&str>) -> Option<String> {
    let (mut socket, _) = connect(server);
    let named = from.map_or(String::new(), |from| format!("X-Forwarded-For: {from}\r\n"));
    let request = format!("GET /livez HTTP/1.1\r\nHost: alt\r\n{named}Connection: close\r\n\r\n");
    socket.write_all(request.as_bytes()).ok()?;
    let mut answer = String::new();
    socket.read_to_string(&mut answer).ok()?;
    answer.lines().next().map(str::to_owned)
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn a_peer_that_is_no_proxy_has_so_many_connections_and_no_more() {
    let mut f = Fixture::new();
    let server = start(&mut f, |_, options| {
        options.trusted_proxies = vec![];
        options.connections_each = 2;
    });
    let held: Vec<_> = (0..2).map(|_| connect(&server)).collect();
    // those two are listened to
    std::thread::sleep(Duration::from_millis(300));
    for _ in 0..3 {
        assert_eq!(alive(&server, None), None);
    }
    drop(held);
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while alive(&server, None).as_deref() != Some("HTTP/1.1 200 OK") {
        assert!(std::time::Instant::now() < until, "no place is given back");
        std::thread::sleep(Duration::from_millis(50));
    }
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
