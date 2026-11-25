//! A push whose body did not come, or had no room, is told so, and not that
//! what it sent is no request.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_untrusted::repository;

/// What a push is answered that says its body has `said` bytes and sends `sent`.
fn pushed(env: &[(&str, &str)], said: usize, sent: usize) -> String {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let mut env = env.to_vec();
    env.push(("ALT_SERVER_REPO", origin.to_str().unwrap()));
    let server = spawn_server_with_env(&env);
    let mut socket = TcpStream::connect(&server.addr).unwrap();
    let wait = Some(Duration::from_secs(30));
    socket.set_read_timeout(wait).unwrap();
    let head = format!(
        "POST /git-receive-pack HTTP/1.1\r\nHost: h\r\nContent-Type: application/x-git-receive-pack-request\r\nContent-Length: {said}\r\n\r\n"
    );
    socket.write_all(head.as_bytes()).unwrap();
    socket.write_all(&vec![b'0'; sent]).unwrap();
    let mut told = String::new();
    let _ = socket.read_to_string(&mut told);
    told
}

#[test]
fn a_push_whose_body_does_not_come_is_told_that_it_took_too_long() {
    let brisk = [
        ("ALT_SERVER_IO_TIMEOUT_MS", "500"),
        ("ALT_SERVER_IO_WINDOW_MS", "500"),
    ];
    let told = pushed(&brisk, 4096, 0);
    assert!(told.starts_with("HTTP/1.1 408 "), "{told}");
}

#[test]
fn a_push_whose_body_has_no_room_is_told_to_come_again() {
    let told = pushed(&[("ALT_SERVER_MAX_RECEIVING_BYTES", "10")], 4096, 4096);
    assert!(told.starts_with("HTTP/1.1 503 "), "{told}");
}
