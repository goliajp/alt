//! The bytes of bodies one caller has the instance hold at once.
use super::bodies::{answer, connect, gone_on, push_of, queued_refs};
use super::slow_bodies::{noise, push_head};
use super::*;
use crate::database::sign_in_deadline::start;
use std::io::Write;

/// An account has a push under way, of which nothing has come yet. A second
/// body of the account that says it has more than a quarter of what the instance
/// holds of bodies is not begun beside it; a device that nobody has signed in
/// for is another caller, and is heard. The push, which is larger than that
/// quarter itself, comes whole while it is the only body of the account, and
/// lands; with it gone the account may begin the body that was turned away.
///
/// What came of the bodies of a caller is counted against the quarter as well;
/// that is held to in the tests of the counting, where nothing races it.
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

    // what a second body of the account that says it has `length` bytes is told
    let second = |length: usize| {
        let (mut socket, mut reader) = connect(&server);
        socket.write_all(push_head(&f, length).as_bytes()).unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
        line
    };
    let over_a_quarter = body.len() / 2 + 1;
    let refused = second(over_a_quarter);
    assert!(refused.starts_with("HTTP/1.1 429"), "{refused}");
    let code = ureq::post(&format!("{}/oauth/device/code", server.url))
        .timeout(Duration::from_secs(30))
        .send_form(&[("client_id", "alt-cli"), ("scope", "unknown")]);
    match code {
        Err(ureq::Error::Status(400, _)) => {}
        other => panic!("{other:?}"),
    }

    // twice the quarter, in pieces: it is the one body its account has
    for piece in body.chunks(body.len() / 8) {
        socket.write_all(piece).unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    let (status, report) = answer(&mut reader);
    assert_eq!(status, "HTTP/1.1 200 OK", "{report}");
    assert!(report.contains("ok refs/heads/queued"), "{report}");
    assert_eq!(queued_refs(&f), 1);
    let begun = second(over_a_quarter);
    assert!(begun.starts_with("HTTP/1.1 100"), "{begun}");
}

/// A body of the account that says nothing of its length is begun beside a push
/// that is under the quarter yet. The push goes on past the quarter and lands:
/// it came first, and the one that came later is held to the quarter.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_body_of_no_length_begun_beside_a_push_does_not_end_the_push() {
    let mut f = Fixture::new();
    let body = push_of(&f, &noise(200_000));
    let all = 2 * body.len() as u64;
    let server = start(&mut f, |_, options| {
        options.receiving_bytes = all;
        options.receiving_each = 8;
        options.body_timeout = Duration::from_secs(120);
    });
    let (mut socket, mut reader) = connect(&server);
    socket
        .write_all(push_head(&f, body.len()).as_bytes())
        .unwrap();
    gone_on(&mut reader);
    let (first, rest) = body.split_at(body.len() / 8);
    socket.write_all(first).unwrap();

    let (mut later, mut later_reader) = connect(&server);
    let chunked = push_head(&f, 0).replace("Content-Length: 0", "Transfer-Encoding: chunked");
    later.write_all(chunked.as_bytes()).unwrap();
    gone_on(&mut later_reader);
    later.write_all(b"4\r\n0000\r\n").unwrap();

    for piece in rest.chunks(rest.len() / 7 + 1) {
        socket.write_all(piece).unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    let (status, report) = answer(&mut reader);
    assert_eq!(status, "HTTP/1.1 200 OK", "{report}");
    assert!(report.contains("ok refs/heads/queued"), "{report}");
    assert_eq!(queued_refs(&f), 1);
}
