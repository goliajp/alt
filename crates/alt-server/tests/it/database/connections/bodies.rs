//! The bodies of callers who are known: they keep no worker while they come, a
//! caller has so many under way, and a request whose body is whole waits for a
//! worker instead of being turned away.
use super::*;
use crate::database::git::{identity, ok, text};
use crate::database::sign_in_deadline::start;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::Command;

fn health(server: &Running) -> u16 {
    match ureq::get(&format!("{}/healthz", server.url)).call() {
        Ok(response) => response.status(),
        Err(ureq::Error::Status(status, _)) => status,
        Err(error) => panic!("{error}"),
    }
}

fn connect(server: &Running) -> (TcpStream, BufReader<TcpStream>) {
    let socket = TcpStream::connect(server.url.strip_prefix("http://").unwrap()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let reader = BufReader::new(socket.try_clone().unwrap());
    (socket, reader)
}

/// The status line of the next response and its body.
fn answer(reader: &mut BufReader<TcpStream>) -> (String, String) {
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    let mut length = 0;
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0, "{status}");
        if line == "\r\n" {
            break;
        }
        let line = line.to_ascii_lowercase();
        if let Some(value) = line.strip_prefix("content-length: ") {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    (
        status.trim_end().to_owned(),
        String::from_utf8_lossy(&body).into_owned(),
    )
}

/// Reads the interim response that tells a client to send its body.
fn gone_on(reader: &mut BufReader<TcpStream>) {
    let mut go_on = String::new();
    reader.read_line(&mut go_on).unwrap();
    assert!(go_on.starts_with("HTTP/1.1 100"), "{go_on}");
    reader.read_line(&mut String::new()).unwrap();
}

/// The head of a fetch request of the token's owner that says it has a body.
fn fetch_head(token: &str) -> String {
    format!(
        "POST /git/repository/git-upload-pack HTTP/1.1\r\nHost: alt\r\nAuthorization: Bearer {token}\r\nGit-Protocol: version=2\r\nContent-Type: application/x-git-upload-pack-request\r\nExpect: 100-continue\r\nContent-Length: 100\r\n\r\n"
    )
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn a_body_that_does_not_come_keeps_no_worker_and_a_caller_has_so_many_under_way() {
    let mut f = Fixture::new();
    let server = start(&mut f, |_, options| options.workers = 1);
    // four bodies of one account, none of which comes
    let mut waiting = Vec::new();
    for _ in 0..4 {
        let (mut socket, mut reader) = connect(&server);
        socket.write_all(fetch_head(&f.token).as_bytes()).unwrap();
        // told to go on: it is authorized, and its body is waited for
        gone_on(&mut reader);
        waiting.push((socket, reader));
    }
    // well within the time a body has
    let until = std::time::Instant::now() + Duration::from_millis(1500);
    while std::time::Instant::now() < until {
        assert_eq!(health(&server), 200, "the one worker is kept");
        std::thread::sleep(Duration::from_millis(25));
    }
    // a fifth of the same account is not begun
    let (mut socket, mut reader) = connect(&server);
    socket.write_all(fetch_head(&f.token).as_bytes()).unwrap();
    let (status, body) = answer(&mut reader);
    assert!(status.starts_with("HTTP/1.1 429"), "{status} {body}");
    assert!(body.contains("\"retry\":\"same_request\""), "{body}");
    drop(waiting);
}

/// A push of one commit to `refs/heads/queued`, as the bytes of its request body.
fn push_body(f: &Fixture) -> Vec<u8> {
    let temp = alt_os::temp::tempdir().unwrap();
    ok(
        temp.path(),
        &f.token,
        &["init", "-q", "--initial-branch=main"],
    );
    identity(temp.path(), &f.token);
    std::fs::write(temp.path().join("file"), "queued\n").unwrap();
    ok(temp.path(), &f.token, &["add", "file"]);
    ok(temp.path(), &f.token, &["commit", "-qm", "queued"]);
    let commit = text(ok(temp.path(), &f.token, &["rev-parse", "HEAD"]));
    let mut packing = Command::new("git")
        .args(["pack-objects", "--stdout", "-q", "--revs"])
        .current_dir(temp.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut revs = packing.stdin.take().unwrap();
    revs.write_all(format!("{commit}\n").as_bytes()).unwrap();
    drop(revs);
    let pack = packing.wait_with_output().unwrap().stdout;
    let mut body = Vec::new();
    let none = "0".repeat(40);
    let command = format!("{none} {commit} refs/heads/queued\0report-status\n");
    alt_wire::pkt::write_data(&mut body, command.as_bytes()).unwrap();
    alt_wire::pkt::write_flush(&mut body).unwrap();
    body.extend_from_slice(&pack);
    body
}

fn queued_refs(f: &Fixture) -> i64 {
    let count = "SELECT count(*) FROM alt_refs WHERE name='refs/heads/queued'";
    f.client().query_one(count, &[]).unwrap().get(0)
}

/// A push is authorized and told to send its body; the one worker is taken by
/// another request meanwhile, which waits for a row the test holds. What the push
/// is answered once its body is whole: after the row is let go of where `freed`,
/// and with the row still held where not.
fn pushed_while_busy(work_timeout: Duration, freed: bool) -> ((String, String), i64) {
    let mut f = Fixture::new();
    let server = start(&mut f, |factory, options| {
        factory.statement_timeout = Duration::from_secs(60);
        options.workers = 1;
        options.work_timeout = work_timeout;
    });
    let body = push_body(&f);
    let (mut socket, mut reader) = connect(&server);
    let head = format!(
        "POST /git/repository/git-receive-pack HTTP/1.1\r\nHost: alt\r\nAuthorization: Bearer {}\r\nContent-Type: application/x-git-receive-pack-request\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n",
        f.token,
        body.len()
    );
    socket.write_all(head.as_bytes()).unwrap();
    // told to go on: it is authorized, and its body is waited for
    gone_on(&mut reader);

    let mut holder = f.client();
    let mut held = Some(holder.transaction().unwrap());
    let row = "SELECT 1 FROM alt_repositories WHERE id='repository' FOR UPDATE";
    held.as_mut().unwrap().query_one(row, &[]).unwrap();
    let (url, token) = (server.url.clone(), f.token.clone());
    let other = std::thread::spawn(move || {
        let request = ureq::get(&format!("{url}/v1/repositories/repository"))
            .set("Authorization", &format!("Bearer {token}"));
        let _ = request.timeout(Duration::from_secs(90)).call();
    });
    // the other request has the worker once its statement waits for the row
    let waits = "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type='Lock'";
    let until = std::time::Instant::now() + Duration::from_secs(30);
    while f.client().query_one(waits, &[]).unwrap().get::<_, i64>(0) == 0 {
        assert!(
            std::time::Instant::now() < until,
            "the worker is never taken"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(health(&server), 503);

    socket.write_all(&body).unwrap();
    if freed {
        // the body is whole and the worker is not free: nothing is answered yet
        socket
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        assert!(
            reader.fill_buf().is_err(),
            "answered while the worker was taken"
        );
        socket
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        held.take().unwrap().commit().unwrap();
    }
    let answered = answer(&mut reader);
    let refs = queued_refs(&f);
    drop(held);
    other.join().unwrap();
    (answered, refs)
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_whose_body_is_whole_waits_for_a_worker_and_lands() {
    let ((status, report), refs) = pushed_while_busy(Duration::from_secs(20), true);
    assert_eq!(status, "HTTP/1.1 200 OK", "{report}");
    assert!(report.contains("ok refs/heads/queued"), "{report}");
    assert_eq!(refs, 1);
}

#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_that_no_worker_comes_free_for_is_told_to_send_itself_again() {
    let ((status, failure), refs) = pushed_while_busy(Duration::from_secs(2), false);
    assert!(status.starts_with("HTTP/1.1 503"), "{status} {failure}");
    assert!(
        failure.contains("\"code\":\"temporarily_unavailable\""),
        "{failure}"
    );
    assert!(failure.contains("\"retry\":\"same_request\""), "{failure}");
    assert_eq!(refs, 0);
}

/// An upload is begun and its connection ends half-way through the content. The
/// request keeps what it reserved while it may still be resumed; once its time is
/// over the server ends it and the room is free, with nobody asking.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn an_upload_whose_connection_ended_gives_back_what_it_reserved_when_its_time_is_over() {
    let f = Fixture::new();
    let server = Running::start(&f);
    let payload = Payload::new("dropped", None);
    let base = "/v1/repositories/repository/epochs/1/requests";
    let begin = alt_server::BeginRequest {
        publication: payload.publication.clone(),
        manifest: payload.manifest.clone(),
    };
    let (status, state) = server.json("POST", base, &f.token, &begin);
    assert_eq!((status, state["state"].as_str()), (200, Some("receiving")));
    let (entry, bytes) = (&payload.manifest.entries[1], &payload.contents[1]);
    let (mut socket, _reader) = connect(&server);
    let head = format!(
        "PUT {base}/owner/dropped/content/{} HTTP/1.1\r\nHost: alt\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n",
        entry.blake3,
        f.token,
        bytes.len()
    );
    socket.write_all(head.as_bytes()).unwrap();
    socket.write_all(&bytes[..bytes.len() / 2]).unwrap();
    drop(socket);

    let held = |f: &Fixture| -> (i64, String) {
        let held = "SELECT r.reserved_bytes,q.state FROM alt_repositories r JOIN alt_requests q ON q.repository=r.id";
        let row = f.client().query_one(held, &[]).unwrap();
        (row.get(0), row.get(1))
    };
    // several rounds of the server's own go by, and it is left alone
    std::thread::sleep(Duration::from_secs(1));
    let (reserved, state) = held(&f);
    assert!(reserved > 0 && state == "receiving", "{reserved} {state}");
    f.client()
        .execute("UPDATE alt_requests SET expires_at=0", &[])
        .unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(30);
    while held(&f) != (0, "expired".to_owned()) {
        assert!(std::time::Instant::now() < until, "{:?}", held(&f));
        std::thread::sleep(Duration::from_millis(50));
    }
}
