//! A fetch that an error of the server stops once the head of its answer
//! went out is told why in the side band of errors: a peer that signed in
//! is told the error, and another only what failed.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_keepalive::{chunked, lines, of_files};

/// What the server says a fetch failed for, to a peer that is not signed in.
const FAILED: &str = "the server failed to make the pack";

/// The answer to a fetch of `head` at `address` under `path`, with the
/// `Authorization` header `signed`, whose pack fails once `gate` is
/// removed: the gate is removed once the head of the answer came.
fn failed(address: &str, (path, signed): (&str, Option<&str>), head: &str, gate: &Path) -> Vec<u8> {
    let pkt = |line: &str| format!("{:04x}{line}", line.len() + 4);
    let want = pkt(&format!("want {head}\n"));
    let command = [
        &pkt("command=fetch\n"),
        "0001",
        &want,
        &pkt("done\n"),
        "0000",
    ]
    .concat();
    let signed = signed.map_or(String::new(), |by| format!("Authorization: {by}\r\n"));
    let request = format!(
        "POST {path}/git-upload-pack HTTP/1.1\r\nHost: h\r\n{signed}Git-Protocol: version=2\r\n\
         Connection: close\r\nContent-Length: {}\r\n\r\n{command}",
        command.len()
    );
    let mut socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    socket.write_all(request.as_bytes()).unwrap();
    let mut answer = BufReader::new(socket);
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

/// What the last line of the body of `answer` says in the side band of
/// errors.
fn told(answer: &[u8]) -> String {
    let (_, body) = chunked(answer);
    let last = lines(&body).last().copied().flatten().unwrap();
    let said = String::from_utf8_lossy(last).into_owned();
    let why = said.strip_prefix('\x03');
    why.unwrap_or_else(|| panic!("{said:?}")).to_owned()
}

/// The pack of a fetch fails after the head of its answer went out with an
/// error that names a file of the server: the answer is a 200 whose body
/// ends in the side band of errors with what failed, and no file. The
/// error itself is in the log of the server.
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
    let why = told(&failed(&server.addr, ("", None), &head, &gate));
    assert_eq!(why, FAILED);
    let log = server.drain_stderr();
    assert!(log.contains(gate.to_str().unwrap()), "{log}");
}

/// A peer that signed in is told the error itself.
#[test]
fn a_fetch_signed_in_that_fails_once_its_head_went_out_is_told_the_error() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    let head = of_files(&root.join("origin"), 4);
    let token = alt_hash::blake3::hash(b"alice-token")
        .to_hex()
        .to_ascii_lowercase();
    std::fs::write(root.join("users"), format!("alice\t{token}\n")).unwrap();
    let gate = tmp.path().join("gate");
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_ROOT", root.to_str().unwrap()),
        ("ALT_SERVER_KEEPALIVE_MS", "1"),
        ("ALT_SERVER_PACK_FAILS", gate.to_str().unwrap()),
    ]);
    let signed = format!("Basic {}", alt_base64::STANDARD.encode("alice:alice-token"));
    let asked = ("/origin", Some(signed.as_str()));
    let why = told(&failed(&server.addr, asked, &head, &gate));
    assert!(why.contains(gate.to_str().unwrap()), "{why}");
    assert!(!why.starts_with(FAILED), "{why}");
}
