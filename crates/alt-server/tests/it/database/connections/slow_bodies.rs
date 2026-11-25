//! Bodies that come slowly: one that goes on is received whole, and one that
//! stalls is given up when the time a body has to move in is over.
use super::bodies::{answer, connect, gone_on, push_of, queued_refs};
use super::*;
use crate::database::sign_in_deadline::start;
use std::io::Write;

/// the time a body has to move in, in these tests
const WINDOW: Duration = Duration::from_millis(200);
/// the least that has to come in it: 1024 bytes a second
const LEAST: usize = 1024 * 200 / 1000;

/// bytes no compression makes fewer
pub(super) fn noise(count: usize) -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 24) as u8
    };
    (0..count).map(|_| next()).collect()
}

pub(super) fn push_head(f: &Fixture, length: usize) -> String {
    format!(
        "POST /git/repository/git-receive-pack HTTP/1.1\r\nHost: alt\r\nAuthorization: Bearer {}\r\nContent-Type: application/x-git-receive-pack-request\r\nExpect: 100-continue\r\nContent-Length: {length}\r\n\r\n",
        f.token
    )
}

/// A push comes at an even pace, as on a slow line, and takes many times as long
/// as a connection is given and as a body has to move in: it is received whole and
/// lands.
///
/// The scale is that of 64 MiB over a line of 256 kbit/s under the time a body
/// has to move in by default, 30 seconds. That line brings 32,000 bytes a second,
/// 31 times the least a body has to bring, and takes 70 such times for the 64 MiB.
/// Here the time is 200 ms, the body comes at 31 times the least for it, and is
/// as long as 70 of them.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_body_as_slow_as_its_line_that_goes_on_is_received_whole_however_long_it_takes() {
    let mut f = Fixture::new();
    let connection = Duration::from_secs(2);
    let server = start(&mut f, |_, options| {
        options.body_timeout = WINDOW;
        options.connection_timeout = connection;
    });
    let body = push_of(&f, &noise(70 * 31 * LEAST));
    assert!(body.len() >= 70 * 31 * LEAST);
    let (mut socket, mut reader) = connect(&server);
    socket
        .write_all(push_head(&f, body.len()).as_bytes())
        .unwrap();
    gone_on(&mut reader);
    let began = std::time::Instant::now();
    // a quarter of what a window brings, four times in each
    for piece in body.chunks(31 * LEAST / 4) {
        socket.write_all(piece).unwrap();
        std::thread::sleep(WINDOW / 4);
    }
    let taken = began.elapsed();
    let (status, report) = answer(&mut reader);
    assert_eq!(status, "HTTP/1.1 200 OK", "{report}");
    assert!(report.contains("ok refs/heads/queued"), "{report}");
    assert_eq!(queued_refs(&f), 1);
    assert!(taken >= 70 * WINDOW && taken >= 5 * connection, "{taken:?}");
}

/// A body that brought enough for a while and then stops is given up when the
/// time it has to move in has passed twice at most, and not before it has passed
/// once.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_body_that_stalls_is_given_up_when_its_time_to_move_in_is_over() {
    let mut f = Fixture::new();
    let window = Duration::from_secs(2);
    let server = start(&mut f, |_, options| {
        options.body_timeout = window;
        options.connection_timeout = Duration::from_secs(60);
    });
    let body = push_of(&f, &noise(100_000));
    let (mut socket, mut reader) = connect(&server);
    socket
        .write_all(push_head(&f, body.len()).as_bytes())
        .unwrap();
    gone_on(&mut reader);
    // far more than a window asks for, and then nothing
    socket.write_all(&body[..50_000]).unwrap();
    let began = std::time::Instant::now();
    let (status, failure) = answer(&mut reader);
    let taken = began.elapsed();
    assert!(status.starts_with("HTTP/1.1 408"), "{status} {failure}");
    assert!(taken >= window - Duration::from_millis(100), "{taken:?}");
    assert!(taken <= 2 * window + Duration::from_secs(2), "{taken:?}");
    assert_eq!(queued_refs(&f), 0);
}
