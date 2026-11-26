//! A fetch whose pack takes long to make hears from the server meanwhile:
//! the head of the answer and the keepalives of `upload-pack`, as `git
//! http-backend` and `git upload-pack` send them.

use std::path::Path;
use std::process::Command;

use crate::altd_server::{alt, ok, spawn_server_with_env};
use crate::altd_server_sending::{answer, asked};
use crate::altd_server_slow_peers::noise;
use crate::altd_server_untrusted::repository;

/// A repository of `files` files that have nothing in common, and its head.
fn of_files(origin: &Path, files: usize) -> String {
    repository(origin);
    let all = noise(files * 8192);
    for (n, file) in all.chunks(8192).enumerate() {
        std::fs::write(origin.join(format!("file-{n}")), file).unwrap();
    }
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "files"]));
    ok(alt(origin, &["rev-parse", "HEAD"])).trim().to_owned()
}

/// The head of an answer, and its body put together from its chunks.
fn chunked(answer: &[u8]) -> (String, Vec<u8>) {
    let ended = answer.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let head = String::from_utf8_lossy(&answer[..ended]).into_owned();
    assert!(
        head.contains("\r\nTransfer-Encoding: chunked\r\n"),
        "{head}"
    );
    assert!(!head.contains("Content-Length"), "{head}");
    let (mut at, mut body) = (ended, Vec::new());
    loop {
        let line = answer[at..].windows(2).position(|w| w == b"\r\n").unwrap();
        let size = std::str::from_utf8(&answer[at..at + line]).unwrap();
        let size = usize::from_str_radix(size, 16).unwrap();
        at += line + 2;
        body.extend_from_slice(&answer[at..at + size]);
        assert_eq!(&answer[at + size..at + size + 2], b"\r\n");
        at += size + 2;
        if size == 0 {
            assert_eq!(at, answer.len());
            return (head, body);
        }
    }
}

/// The status and the bytes of the body the access log of `server` has for
/// its one fetch.
fn logged(server: &mut crate::altd_server::Server) -> (u64, u64) {
    let log = server.drain_stderr();
    let line = log
        .lines()
        .find(|l| l.contains("\"path\":\"/git-upload-pack\""));
    let line = line.unwrap_or_else(|| panic!("no fetch in the log: {log}"));
    let field = |name: &str| {
        let after = line.split(&format!("\"{name}\":")).nth(1).unwrap();
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        digits
            .parse()
            .unwrap_or_else(|_| panic!("{name} in {line}"))
    };
    (field("status"), field("bytes_out"))
}

/// The pkt-lines of `body`, each as its payload; a flush as nothing.
fn lines(body: &[u8]) -> Vec<Option<&[u8]>> {
    let (mut at, mut out) = (0, Vec::new());
    while at < body.len() {
        let size = std::str::from_utf8(&body[at..at + 4]).unwrap();
        let size = usize::from_str_radix(size, 16).unwrap();
        out.push((size >= 4).then(|| &body[at + 4..at + size]));
        at += size.max(4);
    }
    out
}

/// The head and the start of the body go out before the pack is made,
/// and a keepalive behind them until it is; the pack comes whole behind
/// those. A server that sends nothing before its answer is made sends it
/// with its length, and no keepalive.
#[test]
fn the_head_of_a_fetch_goes_out_and_keepalives_follow_while_its_pack_is_made() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    let head = of_files(&origin, 400);
    let mut server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_KEEPALIVE_MS", "1"),
    ]);
    let (told, body) = chunked(&answer(asked(&server.addr, &head)));
    // the log has what the client was sent, keepalives and all
    assert_eq!(logged(&mut server), (200, body.len() as u64));
    assert!(told.starts_with("HTTP/1.1 200 OK\r\n"), "{told}");
    // the request said that the connection ends behind it
    assert!(told.contains("\r\nConnection: close\r\n"), "{told}");
    assert!(
        told.contains("application/x-git-upload-pack-result"),
        "{told}"
    );
    let lines = lines(&body);
    assert_eq!(lines[0], Some(&b"packfile\n"[..]));
    let kept = lines[1..].iter().take_while(|l| *l == &Some(&b"\x01"[..]));
    assert!(kept.count() > 0, "no keepalive before the pack");
    let pack: Vec<u8> = lines[1..]
        .iter()
        .flatten()
        .flat_map(|l| {
            assert_eq!(l[0], 1, "a line of another band");
            l[1..].to_vec()
        })
        .collect();
    assert!(pack.starts_with(b"PACK"));
    assert_eq!(lines.last(), Some(&None));
    assert_eq!(lines.iter().filter(|l| l.is_none()).count(), 1);
}

/// A peer of HTTP/1.0 reads no body sent in chunks: the head that goes out
/// before the pack is made says no length, the body is sent as it is, and
/// it ends where the connection does.
#[test]
fn the_head_of_a_fetch_of_http_1_0_goes_out_with_a_body_that_ends_with_the_connection() {
    use std::io::Write;
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    let head = of_files(&origin, 400);
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_KEEPALIVE_MS", "1"),
    ]);
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
    // keep-alive asked for, which a body without a length cannot give
    let request = format!(
        "POST /git-upload-pack HTTP/1.0\r\nHost: h\r\nGit-Protocol: version=2\r\nConnection: keep-alive\r\nContent-Length: {}\r\n\r\n{command}",
        command.len()
    );
    let mut socket = std::net::TcpStream::connect(&server.addr).unwrap();
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(60)))
        .unwrap();
    socket.write_all(request.as_bytes()).unwrap();
    let got = answer(socket);
    let ended = got.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let (told, body) = got.split_at(ended);
    let told = String::from_utf8_lossy(told);
    assert!(told.starts_with("HTTP/1.1 200 OK\r\n"), "{told}");
    assert!(!told.contains("Transfer-Encoding"), "{told}");
    assert!(!told.contains("Content-Length"), "{told}");
    assert!(told.contains("\r\nConnection: close\r\n"), "{told}");
    let lines = lines(body);
    assert_eq!(lines[0], Some(&b"packfile\n"[..]));
    assert_eq!(lines[1], Some(&b"\x01"[..]), "no keepalive before the pack");
    let pack = lines.iter().flatten().find(|l| l.len() > 1 && l[0] == 1);
    assert!(pack.is_some_and(|l| l[1..].starts_with(b"PACK")));
    assert_eq!(lines.last(), Some(&None));
}

/// What takes longer than a fetch may once its head went out is told in
/// the side band of errors, as `upload-pack` tells what stopped it; what
/// takes longer before that is a 503 as before. Which of the two a time
/// falls in is as fast as the host is, so a few are tried: each answer
/// is one of the two, and the times are such that the later ones fall
/// where the pack is made.
#[test]
fn a_fetch_stopped_once_its_head_went_out_is_told_why_in_the_side_band() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    let head = of_files(&origin, 4000);
    let mut in_band = 0;
    for ms in ["50", "200", "800", "3200"] {
        let mut server = spawn_server_with_env(&[
            ("ALT_SERVER_REPO", origin.to_str().unwrap()),
            ("ALT_SERVER_KEEPALIVE_MS", "1"),
            ("ALT_SERVER_MAX_FETCH_MS", ms),
        ]);
        let got = answer(asked(&server.addr, &head));
        if got.starts_with(b"HTTP/1.1 503") {
            let late = String::from_utf8_lossy(&got);
            assert!(late.ends_with("the fetch takes longer than the server gives one"));
            continue;
        }
        let (_, body) = chunked(&got);
        let why = &b"\x03error: the fetch takes longer than the server gives one"[..];
        // a flush where the pack was made in time
        if let Some(last) = lines(&body).last().unwrap() {
            assert_eq!(*last, why, "{:?}", String::from_utf8_lossy(last));
            in_band += 1;
            // the log has the 200 the client was sent, not the refusal
            assert_eq!(logged(&mut server), (200, body.len() as u64));
        }
    }
    assert!(in_band > 0, "no time fell where the pack is made");
}

/// `alt clone` and `git clone` both take a pack that keepalives come before.
#[test]
fn a_clone_takes_a_pack_that_keepalives_come_before() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    let head = of_files(&origin, 400);
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_KEEPALIVE_MS", "1"),
    ]);
    let url = format!("http://{}/", server.addr);
    ok(alt(tmp.path(), &["clone", &url, "by-alt"]));
    let by_alt = ok(alt(&tmp.path().join("by-alt"), &["rev-parse", "HEAD"]));
    assert_eq!(by_alt.trim(), head);
    assert_eq!(
        std::fs::read(tmp.path().join("by-alt/file-399")).unwrap(),
        std::fs::read(origin.join("file-399")).unwrap()
    );
    let by_git = Command::new("git")
        .args(["clone", "-q", &url])
        .arg(tmp.path().join("by-git"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PROTOCOL", "version=2")
        .output()
        .unwrap();
    assert!(by_git.status.success(), "{by_git:?}");
    assert_eq!(
        std::fs::read(tmp.path().join("by-git/file-399")).unwrap(),
        std::fs::read(origin.join("file-399")).unwrap()
    );
}
