//! The responses to fetches that are written to files take of one room on disk
//! until they are sent, and those of one address a quarter of it.
use super::bodies::connect;
use super::slow_readers::noise;
use super::*;
use crate::database::git::{identity, ok, text};
use crate::database::sign_in_deadline::start;
use alt_server::shares::Shares;
use std::io::{BufRead, Read, Write};

const FILE: usize = 6 << 20;
const ROOM: u64 = 40 << 20;

/// Pushes a branch of two files of noise and one beside it, and gives the tip of
/// the first: a fetch of it alone is of a part of the repository, which is
/// written out object by object.
fn pushed(f: &Fixture, server: &Running) -> String {
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path();
    ok(work, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(work, &f.token);
    for file in 0..2 {
        std::fs::write(work.join(format!("file-{file}")), noise(file, FILE)).unwrap();
    }
    ok(work, &f.token, &["add", "."]);
    ok(work, &f.token, &["commit", "-qm", "noise"]);
    let head = text(ok(work, &f.token, &["rev-parse", "HEAD"]));
    ok(work, &f.token, &["checkout", "-qb", "beside"]);
    ok(
        work,
        &f.token,
        &["commit", "-q", "--allow-empty", "-m", "beside"],
    );
    let url = format!("{}/git/repository", server.url);
    ok(work, &f.token, &["push", "-q", &url, "main", "beside"]);
    head
}

/// A fetch of `head` that the proxy says comes from `from`, sent: its answer is
/// there to be read.
fn asked(
    f: &Fixture,
    server: &Running,
    head: &str,
    from: &str,
) -> std::io::BufReader<std::net::TcpStream> {
    let mut command = Vec::new();
    alt_wire::pkt::write_data(&mut command, b"command=fetch\n").unwrap();
    alt_wire::pkt::write_delim(&mut command).unwrap();
    alt_wire::pkt::write_data(&mut command, format!("want {head}\n").as_bytes()).unwrap();
    alt_wire::pkt::write_data(&mut command, b"done\n").unwrap();
    alt_wire::pkt::write_flush(&mut command).unwrap();
    let head = format!(
        "POST /git/repository/git-upload-pack HTTP/1.1\r\nHost: alt\r\nAuthorization: Bearer {}\r\nX-Forwarded-For: {from}\r\nGit-Protocol: version=2\r\nContent-Type: application/x-git-upload-pack-request\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        f.token,
        command.len()
    );
    let (mut socket, reader) = connect(server);
    socket.write_all(head.as_bytes()).unwrap();
    socket.write_all(&command).unwrap();
    reader
}

/// What a fetch of `head` from `from` is answered with: the status, the seconds
/// after which to come again, the bytes of the body.
fn fetched(f: &Fixture, server: &Running, head: &str, from: &str) -> (u16, Option<u64>, usize) {
    let mut reader = asked(f, server, head, from);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line.split(' ').nth(1).unwrap().parse().unwrap();
    let mut again = None;
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        let Some((name, value)) = line.trim_end().split_once(':') else {
            break;
        };
        if name.eq_ignore_ascii_case("retry-after") {
            again = Some(value.trim().parse().unwrap());
        }
    }
    let mut body = Vec::new();
    reader.read_to_end(&mut body).unwrap();
    (status, again, body.len())
}

/// what the room is left with once the responses that were sent have let go
fn left(room: &Shares) -> u64 {
    let began = std::time::Instant::now();
    while room.held() > 1 && began.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(20));
    }
    room.held()
}

/// The room is forty megabytes, a response twelve. An address that has another
/// taker in the room is held to its quarter, and its fetch is told to come again;
/// the fetch of another address is answered, and so is that of the first once it
/// is the only one of its address. With little left in the room for anybody, a
/// fetch is told to come again whoever it comes from.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn the_responses_to_fetches_that_are_files_take_of_one_room_and_an_address_a_quarter_of_it() {
    let mut f = Fixture::new();
    let room = Shares::passed_by_one_alone(ROOM);
    let server = start(&mut f, |_, options| options.sending = room.clone());
    let head = pushed(&f, &server);
    let (one, other) = ("203.0.113.9", "203.0.113.10");

    let mut mine = room.share(Some(one.parse().unwrap()));
    mine.take(1).unwrap();
    let (status, again, _) = fetched(&f, &server, &head, one);
    assert_eq!((status, again), (503, Some(60)));
    let (status, again, bytes) = fetched(&f, &server, &head, other);
    assert_eq!((status, again), (200, None));
    assert!(bytes >= 2 * FILE, "{bytes}");
    assert_eq!(left(&room), 1);
    // a response that is not taken has its room until the connection is gone
    let mut untaken = asked(&f, &server, &head, other);
    let mut line = String::new();
    untaken.read_line(&mut line).unwrap();
    assert!(line.starts_with("HTTP/1.1 200 "), "{line}");
    std::thread::sleep(Duration::from_secs(1));
    assert!(room.held() >= 2 * FILE as u64, "{}", room.held());
    drop(untaken);
    assert_eq!(left(&room), 1);
    drop(mine);
    let (status, _, alone) = fetched(&f, &server, &head, one);
    assert_eq!((status, alone), (200, bytes));

    let mut most = room.share(None);
    most.take(ROOM - FILE as u64).unwrap();
    assert_eq!(fetched(&f, &server, &head, other).0, 503);
    drop(most);
    assert_eq!(left(&room), 0);
}
