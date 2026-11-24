//! The room the responses to fetches have on the disk between them.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_slow_peers::{listed, noise};
use crate::altd_server_untrusted::repository;

/// A connection that has asked `address` for the whole of the commit `head`.
fn asked(address: &str, head: &str) -> TcpStream {
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
    let mut socket = TcpStream::connect(address).unwrap();
    let wait = Some(Duration::from_secs(60));
    socket.set_read_timeout(wait).unwrap();
    socket.write_all(request.as_bytes()).unwrap();
    socket
}

fn answer(mut socket: TcpStream) -> Vec<u8> {
    let mut all = Vec::new();
    socket.read_to_end(&mut all).unwrap();
    all
}

#[test]
fn a_fetch_whose_response_has_no_room_beside_one_being_sent_is_told_to_come_again() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    // more than the sockets of a connection hold between them
    const BYTES: usize = 16 << 20;
    std::fs::write(origin.join("noise"), noise(BYTES)).unwrap();
    let alt = crate::altd_server::alt;
    crate::altd_server::ok(alt(&origin, &["add", "."]));
    crate::altd_server::ok(alt(&origin, &["commit", "-m", "noise"]));
    // less room than one response takes: alone it is sent all the same
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_MAX_SENDING_BYTES", "1000000"),
    ]);
    let refs = listed(&format!("http://{}/", server.addr)).unwrap();
    let head = refs.split_whitespace().next().unwrap();

    // the first response is begun, so it is written, and no more of it is taken
    let mut first = asked(&server.addr, head);
    let mut begun = [0u8; 12];
    first.read_exact(&mut begun).unwrap();
    assert_eq!(&begun, b"HTTP/1.1 200");

    let second = String::from_utf8_lossy(&answer(asked(&server.addr, head))).into_owned();
    assert!(second.starts_with("HTTP/1.1 503"), "{second:.200}");
    assert!(second.contains("\r\nRetry-After: 60\r\n"), "{second:.400}");
    assert!(second.contains("fetch again"), "{second:.400}");

    // the first is given up by its peer: its room is there again
    drop(first);
    let until = Instant::now() + Duration::from_secs(60);
    let third = loop {
        let third = answer(asked(&server.addr, head));
        if third.starts_with(b"HTTP/1.1 200") {
            break third;
        }
        assert!(
            Instant::now() < until,
            "the room of a response given up is kept"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(third.len() > BYTES, "{} bytes", third.len());
}

/// The server as [`spawn_server_with_env`] starts it, at every address of this host.
fn spawn_at_every_address(env: &[(&str, &str)]) -> (std::process::Child, u16) {
    use std::io::BufRead;
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_altd-server"));
    command
        .args(["--bind", "0.0.0.0:0"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn().unwrap();
    let mut said = std::io::BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    while !line.contains("listening on ") {
        line.clear();
        assert_ne!(said.read_line(&mut line).unwrap(), 0);
    }
    // what the server goes on to say is read and let go of
    std::thread::spawn(move || std::io::copy(&mut said, &mut std::io::sink()));
    let address = line.split("listening on ").nth(1).unwrap();
    let address = address.split_whitespace().next().unwrap();
    (child, address.rsplit(':').next().unwrap().parse().unwrap())
}

/// An address of this host that is not the loopback one.
fn outward() -> std::net::IpAddr {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    // nothing is sent: connecting only has the route chosen
    socket.connect("192.0.2.1:9").unwrap();
    let address = socket.local_addr().unwrap().ip();
    assert!(!address.is_loopback() && !address.is_unspecified());
    address
}

/// The room is eighty megabytes, of which an address has twenty. One address
/// lets a response of sixteen wait: a second fetch of its own is told to come
/// again, while one from this host is answered whole.
#[test]
fn the_responses_one_address_lets_wait_leave_the_room_of_the_others() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    const BYTES: usize = 16 << 20;
    std::fs::write(origin.join("noise"), noise(BYTES)).unwrap();
    let alt = crate::altd_server::alt;
    crate::altd_server::ok(alt(&origin, &["add", "."]));
    crate::altd_server::ok(alt(&origin, &["commit", "-m", "noise"]));
    let (mut server, port) = spawn_at_every_address(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_MAX_SENDING_BYTES", "80000000"),
    ]);
    let (near, far) = (format!("127.0.0.1:{port}"), format!("{}:{port}", outward()));
    let refs = listed(&format!("http://{near}/")).unwrap();
    let head = refs.split_whitespace().next().unwrap();

    let mut first = asked(&far, head);
    let mut begun = [0u8; 12];
    first.read_exact(&mut begun).unwrap();
    assert_eq!(&begun, b"HTTP/1.1 200");
    let second = String::from_utf8_lossy(&answer(asked(&far, head))).into_owned();
    assert!(second.starts_with("HTTP/1.1 503"), "{second:.200}");
    assert!(second.contains("fetch again"), "{second:.400}");
    let other = answer(asked(&near, head));
    assert!(other.starts_with(b"HTTP/1.1 200") && other.len() > BYTES);

    drop(first);
    let until = Instant::now() + Duration::from_secs(60);
    while !answer(asked(&far, head)).starts_with(b"HTTP/1.1 200") {
        assert!(Instant::now() < until, "the room of an address is kept");
        std::thread::sleep(Duration::from_millis(200));
    }
    server.kill().unwrap();
    server.wait().unwrap();
}
