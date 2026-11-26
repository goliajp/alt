//! What a push that an error of the server stops is told in its report: a
//! peer that signed in is told the error, and another only the step that
//! failed and the kind of the error.

use std::path::Path;

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_pack_limits::{push, source};
use crate::altd_server_untrusted::repository;

/// What git says of a push of a new branch from a repository beneath `dir`
/// to `url`.
fn said(dir: &Path, url: &str) -> String {
    let pushed = push(&source(dir, "src"), url, "new");
    let said = String::from_utf8_lossy(&pushed.stderr).into_owned();
    assert!(!pushed.status.success(), "{said}");
    said
}

/// The pack of a push fails to be indexed with an error that names a file
/// of the server: the push is refused with the step and the kind of the
/// error, and the file is named in the log of the server alone
#[test]
fn a_push_whose_pack_an_error_of_the_server_stops_is_told_no_file() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let missing = tmp.path().join("not-there");
    let mut server = spawn_server_with_env(&[
        ("ALT_SERVER_REPO", origin.to_str().unwrap()),
        ("ALT_SERVER_INDEX_FAILS", missing.to_str().unwrap()),
    ]);
    let said = said(tmp.path(), &format!("http://{}/", server.addr));
    assert!(said.contains("index-pack: entity not found"), "{said}");
    assert!(!said.contains(missing.to_str().unwrap()), "{said}");
    let log = server.drain_stderr();
    assert!(log.contains(missing.to_str().unwrap()), "{log}");
}

/// A peer that signed in is told the error itself.
#[test]
fn a_push_signed_in_whose_pack_an_error_of_the_server_stops_is_told_the_error() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    repository(&root.join("origin"));
    let token = alt_hash::blake3::hash(b"alice-token").to_hex();
    std::fs::write(root.join("users"), format!("alice\t{token}\n")).unwrap();
    let missing = tmp.path().join("not-there");
    let server = spawn_server_with_env(&[
        ("ALT_SERVER_ROOT", root.to_str().unwrap()),
        ("ALT_SERVER_INDEX_FAILS", missing.to_str().unwrap()),
    ]);
    let url = format!("http://alice:alice-token@{}/origin/", server.addr);
    let said = said(tmp.path(), &url);
    assert!(said.contains("index-pack: "), "{said}");
    assert!(said.contains(missing.to_str().unwrap()), "{said}");
}
