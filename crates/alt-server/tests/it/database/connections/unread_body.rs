//! A response sent before the request body was read to its end.
use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

fn connect(address: &str) -> (TcpStream, BufReader<TcpStream>) {
    let socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let reader = BufReader::new(socket.try_clone().unwrap());
    (socket, reader)
}
/// the lowercased head of the next response, read past its body
fn head(reader: &mut BufReader<TcpStream>) -> String {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).unwrap();
        assert_ne!(read, 0, "connection ended inside a response: {head:?}");
        if line == "\r\n" {
            break;
        }
        head.push_str(&line.to_ascii_lowercase());
    }
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .map_or(0, |value| value.parse().unwrap());
    reader.read_exact(&mut vec![0; length]).unwrap();
    head
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn refusal_sent_before_the_request_body_arrived_announces_the_close() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let address = server.url.strip_prefix("http://").unwrap();
    let (mut socket, mut reader) = connect(address);
    // without a credential the headers alone decide; the declared body stays unsent
    socket
        .write_all(
            format!(
                "POST /v1/repositories/repository/epochs/1/requests HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
    let refused = head(&mut reader);
    assert!(refused.starts_with("http/1.1 401 "), "{refused}");
    // the body can no longer be told apart from a next request, so the connection
    // ends here; a client that is not told would send its next request into it
    assert!(refused.contains("\nconnection: close\r\n"), "{refused}");
    assert_eq!(reader.read_to_end(&mut Vec::new()).unwrap(), 0);
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn connection_stays_open_after_a_request_whose_body_was_read() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let address = server.url.strip_prefix("http://").unwrap();
    let (mut socket, mut reader) = connect(address);
    socket
        .write_all(
            format!(
                "POST /v1/repositories/repository/epochs/1/requests HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}",
                f.token
            )
            .as_bytes(),
        )
        .unwrap();
    let answered = head(&mut reader);
    assert!(answered.starts_with("http/1.1 400 "), "{answered}");
    assert!(!answered.contains("\nconnection:"), "{answered}");
    socket
        .write_all(format!("GET /livez HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes())
        .unwrap();
    let alive = head(&mut reader);
    assert!(alive.starts_with("http/1.1 200 "), "{alive}");
    assert!(!alive.contains("\nconnection:"), "{alive}");
}
