//! Requests of callers nobody has been told who they are: what they keep of the
//! workers.
use super::*;
use crate::database::sign_in_deadline::start;
use std::io::Write;
use std::net::TcpStream;

fn health(server: &Running) -> u16 {
    match ureq::get(&format!("{}/healthz", server.url)).call() {
        Ok(response) => response.status(),
        Err(ureq::Error::Status(status, _)) => status,
        Err(error) => panic!("{error}"),
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn a_body_that_does_not_come_keeps_no_worker_from_a_caller_not_yet_known() {
    let mut f = Fixture::new();
    let server = start(&mut f, |_, options| options.workers = 1);
    let address = server.url.strip_prefix("http://").unwrap();
    let heads = [
        "POST /oauth/device/code HTTP/1.1\r\nContent-Type: application/x-www-form-urlencoded\r\n",
        "GET /web/config HTTP/1.1\r\n",
    ];
    // each declares a body and sends none of it
    let mut waiting = Vec::new();
    for head in heads {
        let mut socket = TcpStream::connect(address).unwrap();
        let request = format!("{head}Host: {address}\r\nContent-Length: 1\r\n\r\n");
        socket.write_all(request.as_bytes()).unwrap();
        waiting.push(socket);
    }
    // well within the time a body has, and long past the reading of the two heads
    let until = std::time::Instant::now() + Duration::from_millis(1500);
    while std::time::Instant::now() < until {
        assert_eq!(health(&server), 200, "the one worker is kept");
        std::thread::sleep(Duration::from_millis(25));
    }
    drop(waiting);
}
