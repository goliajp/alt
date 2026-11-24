//! The bytes of bodies one caller has the instance hold at once.
use super::bodies::{answer, connect, gone_on, push_of, queued_refs};
use super::slow_bodies::{noise, push_head};
use super::*;
use crate::database::sign_in_deadline::start;
use std::io::Write;

/// An account sends a push slowly and has more of it received than a quarter of
/// what the instance holds of bodies: alone it may. A second body of the account
/// beside it is not begun, however little it says it has; a device that nobody
/// has signed in for is another caller, and is heard. The push goes on to its
/// end and lands, and the account has its place for a second body again.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn the_bodies_one_account_sends_slowly_take_a_quarter_of_the_bytes_and_leave_the_rest() {
    let mut f = Fixture::new();
    let body = push_of(&f, &noise(200_000));
    let all = 2 * body.len() as u64;
    let server = start(&mut f, |_, options| {
        options.receiving_bytes = all;
        options.receiving_each = 8;
        options.body_timeout = Duration::from_secs(120);
    });
    let (mut socket, mut reader) = connect(&server);
    let head = push_head(&f, body.len());
    socket.write_all(head.as_bytes()).unwrap();
    gone_on(&mut reader);
    // three quarters of it, which is three eighths of all the instance holds
    let sent = body.len() / 4 * 3;
    socket.write_all(&body[..sent]).unwrap();

    let second = |length: usize| {
        let (mut socket, mut reader) = connect(&server);
        socket.write_all(push_head(&f, length).as_bytes()).unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
        (line, socket, reader)
    };
    // the body beside it is turned away once what came of the first is counted
    let until = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let (line, ..) = second(1000);
        if line.starts_with("HTTP/1.1 429") {
            break;
        }
        assert!(line.starts_with("HTTP/1.1 100"), "{line}");
        assert!(std::time::Instant::now() < until, "a second body is begun");
        std::thread::sleep(Duration::from_millis(50));
    }
    let code = ureq::post(&format!("{}/oauth/device/code", server.url))
        .timeout(Duration::from_secs(30))
        .send_form(&[("client_id", "alt-cli"), ("scope", "unknown")]);
    match code {
        Err(ureq::Error::Status(400, _)) => {}
        other => panic!("{other:?}"),
    }

    socket.write_all(&body[sent..]).unwrap();
    let (status, report) = answer(&mut reader);
    assert_eq!(status, "HTTP/1.1 200 OK", "{report}");
    assert!(report.contains("ok refs/heads/queued"), "{report}");
    assert_eq!(queued_refs(&f), 1);
    let (line, ..) = second(1000);
    assert!(line.starts_with("HTTP/1.1 100"), "{line}");
}
