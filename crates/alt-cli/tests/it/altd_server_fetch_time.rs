//! How long the server goes on with a fetch: for as long as one may take,
//! and no longer than its peer waits for it.

use std::io::BufRead;
use std::path::Path;

use crate::altd_server::{alt, ok, spawn_server_with_env};
use crate::altd_server_sending::{answer, asked};
use crate::altd_server_slow_peers::noise;
use crate::altd_server_untrusted::repository;

/// A repository of four thousand files that have nothing in common, and its
/// head: a fetch of it reads and packs each of them.
pub(crate) fn of_many_files(origin: &Path) -> String {
    repository(origin);
    let all = noise(4000 * 8192);
    for (n, file) in all.chunks(8192).enumerate() {
        std::fs::write(origin.join(format!("file-{n}")), file).unwrap();
    }
    ok(alt(origin, &["add", "."]));
    ok(alt(origin, &["commit", "-m", "many"]));
    ok(alt(origin, &["rev-parse", "HEAD"])).trim().to_owned()
}

/// A server that gives a fetch a millisecond answers the fetch of four
/// thousand files with a 503 that says why, and does not tell it to come
/// again: it would take as long. One that gives it the time it has by
/// default answers it whole.
#[test]
fn a_fetch_that_takes_longer_than_one_may_is_told_so() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    let head = of_many_files(&origin);
    let short = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_MAX_FETCH_MS", "1"),
    ]);
    let late = String::from_utf8_lossy(&answer(asked(&short.addr, &head))).into_owned();
    assert!(late.starts_with("HTTP/1.1 503"), "{late:.300}");
    assert!(late.ends_with("the fetch takes longer than the server gives one"));
    assert!(!late.contains("Retry-After"), "{late:.300}");
    drop(short);

    let server = spawn_server_with_env(&[("ALT_SERVER_REPO", origin.to_str().unwrap())]);
    let whole = answer(asked(&server.addr, &head));
    assert!(whole.starts_with(b"HTTP/1.1 200") && whole.len() > 4000 * 8192);
}

/// The peer of a fetch closes its connection once it has asked. The server
/// goes on with the fetch no further: what it logs of the request is that
/// its peer is gone, not that it was answered.
#[test]
fn a_fetch_whose_peer_is_gone_is_gone_on_with_no_further() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    let head = of_many_files(&origin);
    let mut server = spawn_server_with_env(&[("ALT_SERVER_REPO", origin.to_str().unwrap())]);
    drop(asked(&server.addr, &head));
    // what the server says until it has logged the request
    let mut said = String::new();
    let mut log = server.stderr.take().unwrap();
    while !said.contains("\"path\":\"/git-upload-pack\"") {
        assert_ne!(log.read_line(&mut said).unwrap(), 0, "{said}");
    }
    assert!(
        said.contains("handler error: the peer of the fetch is gone"),
        "{said}"
    );
    assert!(said.contains("\"status\":500"), "{said}");
}
