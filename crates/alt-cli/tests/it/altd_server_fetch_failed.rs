//! A fetch that an error of the server stops once the head of its answer
//! went out is told why in the side band of errors.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::altd_server::{Server, spawn_server_with_env};
use crate::altd_server_keepalive::{chunked, lines, of_files};
use crate::altd_server_sending::asked;

/// The answer to a fetch of `head` from `server`, whose pack fails once
/// `gate` is removed: the gate is removed once the head of the answer came.
fn failed(server: &Server, head: &str, gate: &Path) -> Vec<u8> {
    let mut answer = BufReader::new(asked(&server.addr, head));
    let mut status = String::new();
    answer.read_line(&mut status).unwrap();
    assert_eq!(status, "HTTP/1.1 200 OK\r\n");
    let until = Instant::now() + Duration::from_secs(60);
    while !gate.exists() {
        assert!(Instant::now() < until, "the pack was never begun");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::remove_file(gate).unwrap();
    let mut all = status.into_bytes();
    answer.read_to_end(&mut all).unwrap();
    all
}

/// The pack of a fetch fails after the head of its answer went out: the
/// answer is a 200 whose body ends in the side band of errors with the
/// reason, and the server logs the error.
#[test]
fn a_fetch_that_fails_once_its_head_went_out_is_told_why_in_the_side_band() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    let head = of_files(&origin, 4);
    let gate = tmp.path().join("gate");
    let mut server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_KEEPALIVE_MS", "1"),
        ("ALT_SERVER_PACK_FAILS", gate.to_str().unwrap()),
    ]);
    let (_, body) = chunked(&failed(&server, &head, &gate));
    let last = lines(&body).last().copied().flatten().unwrap();
    let said = String::from_utf8_lossy(last).into_owned();
    assert!(said.starts_with('\x03') && said.len() > 1, "{said:?}");
    let log = server.drain_stderr();
    assert!(log.contains(gate.to_str().unwrap()), "{log}");
}
