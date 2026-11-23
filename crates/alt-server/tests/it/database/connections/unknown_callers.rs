//! Requests of callers nobody has been told who they are: what they keep of the
//! workers, and which address their sign-in attempts are counted for.
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

fn device_code(server: &Running, forwarded: &str) -> u16 {
    let request = ureq::post(&format!("{}/oauth/device/code", server.url))
        .set("Content-Type", "application/x-www-form-urlencoded")
        .set("X-Forwarded-For", forwarded);
    match request.send_string("client_id=alt-cli&scope=account") {
        Ok(response) => response.status(),
        Err(ureq::Error::Status(status, _)) => status,
        Err(error) => panic!("{error}"),
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn sign_in_attempts_are_counted_for_the_address_the_proxy_names() {
    let mut f = Fixture::new();
    let server = start(&mut f, |_, _| ());
    assert_eq!(device_code(&server, "203.0.113.7"), 200);
    let key = |address: &str| format!("network:{}", alt_browser_auth::digest(address));
    let spent = f
        .client()
        .execute(
            "UPDATE alt_login_budgets SET attempts=100 WHERE key=$1",
            &[&key("203.0.113.7")],
        )
        .unwrap();
    assert_eq!(spent, 1, "no attempt was counted for the address named");
    assert_eq!(device_code(&server, "203.0.113.7"), 503);
    // what the sender wrote comes first; the proxy names whom it heard from last
    assert_eq!(device_code(&server, "203.0.113.7, 203.0.113.8"), 200);
    assert_eq!(device_code(&server, "203.0.113.8,203.0.113.7"), 503);
}
