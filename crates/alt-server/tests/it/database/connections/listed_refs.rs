//! A list of references too long for memory is a file of the room the responses
//! to fetches have, as the pack of a fetch is.
use super::bodies::connect;
use super::*;
use crate::database::git::{identity, ok};
use crate::database::sign_in_deadline::start;
use alt_server::shares::Shares;
use std::io::{BufRead, Read, Write};

const ROOM: u64 = 24 << 20;
/// as many references of names as long as a name may be list as nine megabytes
const REFS: usize = 9000;

/// Pushes `REFS` branches of long names at one commit.
fn pushed(f: &Fixture, server: &Running) {
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path();
    ok(work, &f.token, &["init", "-q", "--initial-branch=main"]);
    identity(work, &f.token);
    std::fs::write(work.join("file"), b"listed\n").unwrap();
    ok(work, &f.token, &["add", "."]);
    ok(work, &f.token, &["commit", "-qm", "listed"]);
    let part = "n".repeat(240);
    let mut updating = std::process::Command::new("git")
        .args(["update-ref", "--stdin"])
        .current_dir(work)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = updating.stdin.take().unwrap();
    for at in 0..REFS {
        let name = format!("refs/heads/{at:05}/{part}/{part}/{part}/{part}");
        writeln!(lines, "create {name} HEAD").unwrap();
    }
    drop(lines);
    assert!(updating.wait().unwrap().success());
    let url = format!("{}/git/repository", server.url);
    ok(work, &f.token, &["push", "-q", "--mirror", &url]);
}

/// What a request for the references that the proxy says comes from `from` is
/// answered with: the status, the seconds after which to come again, the bytes
/// of the body. `pushing` asks as a push does before it sends, and otherwise as
/// a fetch does.
fn listed(
    f: &Fixture,
    server: &Running,
    from: &str,
    pushing: bool,
) -> (u16, Option<u64>, usize, String) {
    let mut command = Vec::new();
    alt_wire::pkt::write_data(&mut command, b"command=ls-refs\n").unwrap();
    alt_wire::pkt::write_delim(&mut command).unwrap();
    alt_wire::pkt::write_flush(&mut command).unwrap();
    let common = format!(
        "Host: alt\r\nAuthorization: Bearer {}\r\nX-Forwarded-For: {from}\r\nConnection: close\r\n",
        f.token
    );
    let head = match pushing {
        true => format!(
            "GET /git/repository/info/refs?service=git-receive-pack HTTP/1.1\r\n{common}\r\n"
        ),
        false => format!(
            "POST /git/repository/git-upload-pack HTTP/1.1\r\n{common}Git-Protocol: version=2\r\nContent-Type: application/x-git-upload-pack-request\r\nContent-Length: {}\r\n\r\n",
            command.len()
        ),
    };
    let (mut socket, mut reader) = connect(server);
    socket.write_all(head.as_bytes()).unwrap();
    if !pushing {
        socket.write_all(&command).unwrap();
    }
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
    let begins = String::from_utf8_lossy(&body[..body.len().min(300)]).into_owned();
    (status, again, body.len(), begins)
}

/// The room is twenty-four megabytes and the references list as more than nine.
/// An address that has another taker in the room is held to its quarter, and is
/// told to come again for its list, asked for as a fetch or as a push asks; the
/// list of another address is given, and the room is given back when it is sent.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_long_list_of_references_takes_of_the_room_for_responses_and_an_address_a_quarter_of_it() {
    let mut f = Fixture::new();
    let room = Shares::passed_by_one_alone(ROOM);
    let server = start(&mut f, |factory, options| {
        factory.limits.ref_updates = 2 * REFS;
        factory.limits.request_bytes = 64 << 20;
        factory.statement_timeout = Duration::from_secs(300);
        options.work_timeout = Duration::from_secs(300);
        options.sending = room.clone();
    });
    pushed(&f, &server);
    let (one, other) = ("203.0.113.9", "203.0.113.10");
    let mut mine = room.share(Some(one.parse().unwrap()));
    mine.take(1).unwrap();
    for pushing in [false, true] {
        let (status, again, _, said) = listed(&f, &server, one, pushing);
        assert_eq!((status, again), (503, Some(60)), "{pushing} {said}");
        let (status, again, bytes, said) = listed(&f, &server, other, pushing);
        assert_eq!((status, again), (200, None), "{pushing} {said}");
        assert!(bytes > 9_000_000, "{bytes}");
        let began = std::time::Instant::now();
        while room.held() > 1 {
            assert!(began.elapsed() < Duration::from_secs(30), "{}", room.held());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
