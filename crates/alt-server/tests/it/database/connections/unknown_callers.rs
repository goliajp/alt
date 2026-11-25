//! Requests of callers nobody has been told who they are: what they keep of the
//! workers, and which address their sign-in attempts are counted for.
use super::*;
use crate::database::sign_in_deadline::start;
use std::io::Write;
use std::net::TcpStream;

fn health(server: &Running) -> u16 {
    let asked = ureq::get(&format!("{}/healthz", server.url));
    match asked.timeout(Duration::from_secs(30)).call() {
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

/// the status a device that asks for a code is answered with, and after how many
/// seconds it is told to ask again
fn device_code_answer(server: &Running, forwarded: &str) -> (u16, Option<u64>) {
    let request = ureq::post(&format!("{}/oauth/device/code", server.url))
        .set("Content-Type", "application/x-www-form-urlencoded")
        .set("X-Forwarded-For", forwarded);
    let response = match request.send_string("client_id=alt-cli&scope=account") {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("{error}"),
    };
    let again = response.header("Retry-After").map(|v| v.parse().unwrap());
    (response.status(), again)
}

fn device_code(server: &Running, forwarded: &str) -> u16 {
    device_code_answer(server, forwarded).0
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn sign_in_attempts_are_counted_for_the_address_the_proxy_names() {
    let mut f = Fixture::new();
    let server = start(&mut f, |_, _| ());
    assert_eq!(device_code(&server, "203.0.113.7"), 200);
    let key = |address: &str| format!("code:{}", alt_browser_auth::digest(address));
    let spent = f
        .client()
        .execute(
            "UPDATE alt_login_budgets SET attempts=100 WHERE key=$1",
            &[&key("203.0.113.7")],
        )
        .unwrap();
    assert_eq!(spent, 1, "no attempt was counted for the address named");
    // refused, and told when the minute its codes were counted in is over
    let (status, again) = device_code_answer(&server, "203.0.113.7");
    assert_eq!(status, 503);
    assert!(
        again.is_some_and(|seconds| (1..=60).contains(&seconds)),
        "{again:?}"
    );
    // what the sender wrote comes first; the proxy names whom it heard from last
    assert_eq!(device_code(&server, "203.0.113.7, 203.0.113.8"), 200);
    assert_eq!(device_code(&server, "203.0.113.8,203.0.113.7"), 503);
}

/// With no proxy named every caller is the peer, one address for all of them. The
/// bodies that address may have under way are taken by requests that send none of
/// theirs: a request that brings no body is answered all the same, and one that
/// brings one is told to come later.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn a_request_without_a_body_takes_no_place_among_the_bodies_being_received() {
    let mut f = Fixture::new();
    let server = start(&mut f, |_, options| {
        options.trusted_proxies.clear();
        options.receiving_each = 2;
    });
    let address = server.url.strip_prefix("http://").unwrap();
    let head =
        "POST /oauth/device/code HTTP/1.1\r\nContent-Type: application/x-www-form-urlencoded\r\n";
    let mut waiting = Vec::new();
    for _ in 0..2 {
        let mut socket = TcpStream::connect(address).unwrap();
        let request = format!("{head}Host: {address}\r\nContent-Length: 30\r\n\r\n");
        socket.write_all(request.as_bytes()).unwrap();
        waiting.push(socket);
    }
    let status = |response: std::result::Result<ureq::Response, ureq::Error>| match response {
        Ok(response) => response.status(),
        Err(ureq::Error::Status(status, _)) => status,
        Err(error) => panic!("{error}"),
    };
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build();
    // the two are under way once a third body is refused its place
    let until = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        // the endpoint of the two answers a caller that has its bodies under
        // way as it answers when it is busy
        let third = agent.post(&format!("{}/oauth/device/code", server.url));
        let third = third.set("Content-Type", "application/x-www-form-urlencoded");
        if status(third.send_string("client_id=alt-cli&scope=account")) == 503 {
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "no body is refused its place"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    for _ in 0..3 {
        let bodiless = agent.get(&format!("{}/web/config", server.url)).call();
        assert_ne!(status(bodiless), 429);
    }
    drop(waiting);
}
