//! Responses that are read slowly: one whose reader goes on is sent whole, and
//! one whose reader stops is given up when the time it has to move in is over.
use super::bodies::connect;
use super::*;
use crate::database::git::{identity, ok, text};
use crate::database::sign_in_deadline::start;
use std::io::{Read, Write};

/// far more than the sockets of a loopback connection hold between them
const FILES: usize = 6;
const FILE: usize = 8 << 20;

/// bytes no compression makes fewer, and no two files share
fn noise(file: usize, count: usize) -> Vec<u8> {
    let mut state =
        0x9e37_79b9_7f4a_7c15_u64 ^ (file as u64 + 1).wrapping_mul(0xff51_afd7_ed55_8ccd);
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 24) as u8
    };
    (0..count).map(|_| next()).collect()
}

/// Pushes a repository of noise and gives the request that clones it over one
/// connection that is closed behind the response.
fn clone_request(f: &Fixture, server: &Running) -> Vec<u8> {
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path();
    ok(work, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(work, &f.token);
    for file in 0..FILES {
        std::fs::write(work.join(format!("file-{file}")), noise(file, FILE)).unwrap();
    }
    ok(work, &f.token, &["add", "."]);
    ok(work, &f.token, &["commit", "-qm", "noise"]);
    let url = format!("{}/git/repository", server.url);
    ok(work, &f.token, &["push", "-q", &url, "main"]);
    let head = text(ok(work, &f.token, &["rev-parse", "HEAD"]));
    let mut command = Vec::new();
    alt_wire::pkt::write_data(&mut command, b"command=fetch\n").unwrap();
    alt_wire::pkt::write_delim(&mut command).unwrap();
    alt_wire::pkt::write_data(&mut command, format!("want {head}\n").as_bytes()).unwrap();
    alt_wire::pkt::write_data(&mut command, b"done\n").unwrap();
    alt_wire::pkt::write_flush(&mut command).unwrap();
    let mut request = format!(
        "POST /git/repository/git-upload-pack HTTP/1.1\r\nHost: alt\r\nAuthorization: Bearer {}\r\nGit-Protocol: version=2\r\nContent-Type: application/x-git-upload-pack-request\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        f.token,
        command.len()
    )
    .into_bytes();
    request.extend(command);
    request
}

/// What comes of the response until the connection ends, read `piece` bytes at a
/// time with `rest` between two pieces; after the first `before` bytes the reader
/// stops for `stop`.
fn read(
    server: &Running,
    request: &[u8],
    (piece, rest): (usize, Duration),
    (before, stop): (usize, Duration),
) -> usize {
    let (mut socket, _) = connect(server);
    socket.write_all(request).unwrap();
    let mut buffer = vec![0; piece];
    let (mut all, mut stopped) = (0, false);
    loop {
        match socket.read(&mut buffer) {
            Ok(0) | Err(_) => return all,
            Ok(count) => all += count,
        }
        if !stopped && all >= before {
            stopped = true;
            std::thread::sleep(stop);
        }
        std::thread::sleep(rest);
    }
}

/// A clone is read at an even pace, as over a slow line, and takes many times as
/// long as a connection is given: it is sent whole.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_response_read_as_slowly_as_its_line_that_goes_on_is_sent_whole_however_long_it_takes() {
    let mut f = Fixture::new();
    let connection = Duration::from_secs(1);
    let server = start(&mut f, |_, options| {
        // the push that fills the repository pauses while Git packs it
        options.body_timeout = Duration::from_secs(2);
        options.connection_timeout = connection;
    });
    let request = clone_request(&f, &server);
    let none = (usize::MAX, Duration::ZERO);
    let whole = read(&server, &request, (1 << 20, Duration::ZERO), none);
    assert!(whole >= FILES * FILE, "{whole}");
    // 256 KiB twenty times a second
    let began = std::time::Instant::now();
    let slowly = read(
        &server,
        &request,
        (256 << 10, Duration::from_millis(50)),
        none,
    );
    let taken = began.elapsed();
    assert_eq!(slowly, whole);
    assert!(taken >= 8 * connection, "{taken:?}");
}

/// A reader that took a part of the response and then takes nothing is given up
/// when the time a response has to move in has passed twice at most, long before
/// the connection would be: what it finds when it reads again is less than the
/// whole.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_response_whose_reader_stops_is_given_up_when_its_time_to_move_in_is_over() {
    let mut f = Fixture::new();
    let window = Duration::from_secs(2);
    let server = start(&mut f, |_, options| {
        options.body_timeout = window;
        options.connection_timeout = Duration::from_secs(120);
    });
    let request = clone_request(&f, &server);
    let fast = (1 << 20, Duration::ZERO);
    let whole = read(&server, &request, fast, (usize::MAX, Duration::ZERO));
    assert!(whole >= FILES * FILE, "{whole}");
    let stopping = (1 << 20, 2 * window + Duration::from_secs(2));
    let began = std::time::Instant::now();
    let found = read(&server, &request, fast, stopping);
    assert!(found < whole, "{found} of {whole}");
    assert!(began.elapsed() < Duration::from_secs(60));
}
