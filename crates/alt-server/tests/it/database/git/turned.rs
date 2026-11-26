//! A large response to a fetch that was put off to wait for its turn chooses
//! its objects again once it has it, in a time of its own: what it took to
//! choose them first and to wait leaves it no less.

use super::given_up::history;
use super::*;
use alt_server::shares::Shares;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Instant;

/// A fetch of `want` that the proxy says comes from `from`, sent: its answer
/// is there to be read.
fn asked(url: &str, token: &str, want: &str, from: &str) -> BufReader<TcpStream> {
    let mut command = Vec::new();
    alt_wire::pkt::write_data(&mut command, b"command=fetch\n").unwrap();
    alt_wire::pkt::write_delim(&mut command).unwrap();
    alt_wire::pkt::write_data(&mut command, format!("want {want}\n").as_bytes()).unwrap();
    alt_wire::pkt::write_data(&mut command, b"done\n").unwrap();
    alt_wire::pkt::write_flush(&mut command).unwrap();
    let head = format!(
        "POST /git/repository/git-upload-pack HTTP/1.1\r\nHost: alt\r\nAuthorization: Bearer {token}\r\nX-Forwarded-For: {from}\r\nGit-Protocol: version=2\r\nContent-Type: application/x-git-upload-pack-request\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        command.len()
    );
    let mut socket = TcpStream::connect(url.strip_prefix("http://").unwrap()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(1200)))
        .unwrap();
    socket.write_all(head.as_bytes()).unwrap();
    socket.write_all(&command).unwrap();
    BufReader::new(socket)
}

/// The status of an answer, its head read.
fn begun(reader: &mut BufReader<TcpStream>) -> u16 {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line.split(' ').nth(1).unwrap().parse().unwrap();
    while line != "\r\n" {
        line.clear();
        reader.read_line(&mut line).unwrap();
    }
    status
}

/// The status of an answer and the bytes of its body.
fn answered(mut reader: BufReader<TcpStream>) -> (u16, usize) {
    let status = begun(&mut reader);
    let mut body = Vec::new();
    reader.read_to_end(&mut body).unwrap();
    (status, body.len())
}

/// For `git fast-import`, after a history: a commit of a file of twelve
/// megabytes of noise, and one after it. A response of the first is written
/// to a file, past what a response keeps in memory, and takes of the room.
fn noise() -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut bytes = Vec::with_capacity(12 << 20);
    while bytes.len() < 12 << 20 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.extend_from_slice(&state.to_le_bytes());
    }
    let who = "committer Original Actor <original@example.test> 1800000000 +0000";
    let mut stream = format!("commit refs/heads/main\n{who}\ndata 2\nn\n").into_bytes();
    write!(stream, "M 100644 inline noise\ndata {}\n", bytes.len()).unwrap();
    stream.extend_from_slice(&bytes);
    write!(stream, "\ncommit refs/heads/main\n{who}\ndata 2\ne\n\n").unwrap();
    stream
}

/// The repository fetched: `ALT_TEST_TURNED_REPOSITORY`, a Git repository
/// whose `main` is fetched but for its last commit, where that is set; a
/// history of large trees and a file of noise where it is not. Its tips are pushed with `push`,
/// and the one that is fetched is given.
fn fetched(work: &Path, token: &str, push: &dyn Fn(&Path)) -> String {
    let from = match std::env::var_os("ALT_TEST_TURNED_REPOSITORY") {
        Some(repository) => std::path::PathBuf::from(repository),
        None => {
            ok(work, token, &["init", "-q", "--initial-branch=main"]);
            let mut importing = Command::new("git")
                .args(["fast-import", "--quiet"])
                .current_dir(work)
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut stream = importing.stdin.take().unwrap();
            let written = stream.write_all(&[history(8000, 3000), noise()].concat());
            drop(stream);
            assert!(importing.wait().unwrap().success());
            written.unwrap();
            work.to_path_buf()
        }
    };
    ok(&from, token, &["branch", "-f", "older", "main~1"]);
    push(&from);
    text(ok(&from, token, &["rev-parse", "older"]))
}

/// Two addresses fetch most of a history whose objects take a while to be
/// chosen, and the response is large: the first keeps its turn, unread, and
/// the second is put off once it chose its objects. It is let have its turn
/// where less is left of the time of its request than choosing them takes,
/// and still is answered, whole: it chooses them again in a time of its own.
/// Each request has three times what a fetch alone takes, unless
/// `ALT_TEST_TURNED_TIME_MS` says how long.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_fetch_put_off_for_its_turn_chooses_its_objects_again_in_a_time_of_its_own() {
    let f = Fixture::new();
    // a quota the largest history this is run with fits in
    let quota = "UPDATE alt_repositories SET quota_bytes=1099511627776 WHERE id='repository'";
    f.client().batch_execute(quota).unwrap();
    let parts = |time: Duration, room: &Arc<Shares>| {
        let (mut factory, mut options) = Running::parts(&f, None, vec![]);
        factory.limits.request_bytes = 1 << 30;
        factory.limits.graph_objects = 1 << 24;
        factory.limits.graph_edges = 1 << 30;
        factory.limits.structured_bytes = 1 << 31;
        factory.limits.tree_paths = 1 << 24;
        factory.limits.walk_steps = 1 << 30;
        factory.limits.object_bytes = 1 << 30;
        factory.limits.total_bytes = 1 << 36;
        factory.statement_timeout = Duration::from_secs(300);
        options.native.portable = None;
        options.workers = 1;
        options.work_timeout = time;
        options.body_timeout = Duration::from_secs(600);
        options.sending = room.clone();
        (factory, options)
    };
    let temp = alt_os::temp::tempdir().unwrap();
    let work = temp.path();
    let long = Duration::from_secs(600);
    let (factory, options) = parts(long, &Shares::passed_by_one_alone(u64::MAX, 1));
    let server = Running::serve(factory, options, None, false);
    let url = format!("{}/git/repository", server.url);
    let push = |from: &Path| {
        ok(from, &f.token, &["push", "-q", &url, "main", "older"]);
    };
    let older = fetched(work, &f.token, &push);
    let began = Instant::now();
    let (status, whole) = answered(asked(&server.url, &f.token, &older, "203.0.113.8"));
    let alone = began.elapsed();
    assert_eq!(status, 200);
    assert!(
        alone >= Duration::from_secs(2),
        "too short to tell: {alone:?}"
    );
    drop(server);

    let time = match std::env::var("ALT_TEST_TURNED_TIME_MS") {
        Ok(time) => Duration::from_millis(time.parse().unwrap()),
        Err(_) => alone * 3,
    };
    // a part of half the response: it is large, and the room has it alone
    let room = Shares::passed_by_one_alone(whole as u64 * 2, 1);
    let (factory, options) = parts(time, &room);
    let server = Running::serve(factory, options, None, false);
    let url = server.url.clone();
    let mut kept = asked(&url, &f.token, &older, "203.0.113.9");
    assert_eq!(begun(&mut kept), 200);
    let began = Instant::now();
    let waiter = {
        let (url, token, older) = (url.clone(), f.token.clone(), older.clone());
        std::thread::spawn(move || answered(asked(&url, &token, &older, "203.0.113.10")))
    };
    while room.waiting() != 1 {
        let (since, waiting) = (began.elapsed(), room.waiting());
        assert!(
            !waiter.is_finished(),
            "answered after {since:?}, alone {alone:?}"
        );
        assert!(
            since < time,
            "{waiting} wait after {since:?}, alone {alone:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let put_off = began.elapsed();
    // less is left of its time than its objects took to be chosen
    let late = time.saturating_sub(alone / 2).max(put_off);
    std::thread::sleep(late - put_off);
    let let_in = began.elapsed();
    drop(kept);
    let (status, bytes) = waiter.join().unwrap();
    let ended = began.elapsed();
    assert_eq!(
        (status, bytes),
        (200, whole),
        "put off after {put_off:?}, let in after {let_in:?}, answered after {ended:?} of {time:?}; alone {alone:?}"
    );
    eprintln!(
        "alone {alone:?}, time {time:?}: put off after {put_off:?}, let in after {let_in:?}, answered after {ended:?}"
    );
}
