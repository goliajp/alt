//! Readers of a clone from `altd-server` that take it slowly: one that goes
//! on is sent all of it, one that stops is let go of.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_slow_peers::{listed, noise};
use crate::altd_server_untrusted::repository;

/// How much of a clone comes when, from the first of the pack on, it is read
/// `piece` bytes at a time with `rest` between two pieces for `slowly`, and then
/// as fast as it comes; the window a response has to move in is `window_ms`. And
/// the bytes of the file cloned. Where `kept`, the head of the response goes out
/// while the pack is made, keepalives behind it, and the pack in chunks behind
/// those; where not, the head goes out with the pack.
pub(crate) fn cloned_slowly(
    window_ms: &str,
    (piece, rest): (usize, Duration),
    slowly: Duration,
    kept: bool,
) -> (usize, usize) {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    // far more than the sockets of the connection hold between them
    const BYTES: usize = 48 << 20;
    std::fs::write(origin.join("noise"), noise(BYTES)).unwrap();
    let alt = crate::altd_server::alt;
    crate::altd_server::ok(alt(&origin, &["add", "."]));
    crate::altd_server::ok(alt(&origin, &["commit", "-m", "noise"]));
    let keepalive = if kept { "1" } else { "3600000" };
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_IO_WINDOW_MS", window_ms),
        ("ALT_SERVER_KEEPALIVE_MS", keepalive),
    ]);
    let refs = listed(&format!("http://{}/", server.addr)).unwrap();
    let head = refs.split_whitespace().next().unwrap();
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
    let request = format!(
        "POST /git-upload-pack HTTP/1.1\r\nHost: h\r\nGit-Protocol: version=2\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{command}",
        command.len()
    );
    let mut socket = TcpStream::connect(&server.addr).unwrap();
    let wait = Some(Duration::from_secs(120));
    socket.set_read_timeout(wait).unwrap();
    socket.write_all(request.as_bytes()).unwrap();
    // the pack is being sent from here on
    let mut begun = Vec::new();
    while !begun.windows(4).any(|w| w == b"PACK") {
        let mut more = [0u8; 4096];
        let count = socket.read(&mut more).unwrap();
        assert_ne!(count, 0, "no pack in {}", String::from_utf8_lossy(&begun));
        begun.extend_from_slice(&more[..count]);
    }
    let chunked = begun
        .windows(26)
        .any(|w| w == b"Transfer-Encoding: chunked");
    assert!(begun.starts_with(b"HTTP/1.1 200") && chunked == kept);
    let mut buffer = vec![0u8; piece];
    let mut read = begun.len();
    let began = std::time::Instant::now();
    while began.elapsed() < slowly {
        match socket.read(&mut buffer) {
            Ok(0) | Err(_) => return (read, BYTES),
            Ok(count) => read += count,
        }
        std::thread::sleep(rest);
    }
    let mut rest = Vec::new();
    let _ = socket.read_to_end(&mut rest);
    (read + rest.len(), BYTES)
}

/// A response has to move 1 KiB a second over each window of two seconds. A
/// reader takes 128 KiB a second, evenly, for eight windows. The sockets hold
/// megabytes, of which a third has to be free before a socket that is waited for
/// tells that it can be written to: that happens less than once in a window. The
/// server goes on all the same, and the rest comes whole.
#[test]
fn a_clone_read_slowly_and_evenly_goes_on_for_many_windows() {
    let pace = (32 << 10, Duration::from_millis(250));
    let (read, whole) = cloned_slowly("2000", pace, Duration::from_secs(16), false);
    assert!(read > whole, "{read} bytes of more than {whole}");
}

/// A reader that takes nothing for longer than two windows is let go of: what it
/// finds when it reads again is less than the whole.
#[test]
fn a_clone_whose_reader_stops_for_two_windows_is_given_up() {
    let pace = (1, Duration::from_secs(10));
    let (read, whole) = cloned_slowly("3000", pace, Duration::from_secs(10), false);
    assert!(read < whole, "{read} bytes of {whole}");
}

/// The same reader of a clone whose head went out while its pack was made,
/// and whose pack goes out in chunks behind the keepalives, is let go of too.
#[test]
fn a_clone_sent_in_chunks_whose_reader_stops_for_two_windows_is_given_up() {
    let pace = (1, Duration::from_secs(10));
    let (read, whole) = cloned_slowly("3000", pace, Duration::from_secs(10), true);
    assert!(read < whole, "{read} bytes of {whole}");
}
