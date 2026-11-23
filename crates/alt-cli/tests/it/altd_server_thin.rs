//! A push as Git sends it: thin, a changed file as a delta of the
//! version the server has, which is not in the pack.

use std::fs;

use crate::altd_server::{alt, ok, spawn_server_with_env};
use crate::altd_server_pack_limits::{git, push, source};

#[test]
fn a_push_of_a_small_change_to_a_large_file_is_taken() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    fs::create_dir(&origin).unwrap();
    ok(alt(&origin, &["init", "."]));
    let server = spawn_server_with_env(&[("ALT_SERVER_REPO", origin.to_str().unwrap())]);
    let url = format!("http://{}/", server.addr);

    let src = source(tmp.path(), "work");
    let line = "a line of a file that is large enough to be sent as a delta";
    let large: String = (0..20_000).map(|n| format!("{n} {line}\n")).collect();
    fs::write(src.join("large.txt"), &large).unwrap();
    assert!(git(&src, &["add", "."]).status.success());
    assert!(git(&src, &["commit", "-q", "-m", "large"]).status.success());
    let first = push(&src, &url, "main");
    assert!(first.status.success(), "{first:?}");

    fs::write(src.join("large.txt"), format!("{large}one more line\n")).unwrap();
    assert!(
        git(&src, &["commit", "-q", "-am", "a line more"])
            .status
            .success()
    );
    // Git sends the new version as a delta of the first, and not the first
    let second = push(&src, &url, "main");
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );

    let clone = tmp.path().join("clone");
    let cloned = git(tmp.path(), &["clone", "-q", &url, clone.to_str().unwrap()]);
    assert!(cloned.status.success(), "{cloned:?}");
    let read = fs::read_to_string(clone.join("large.txt")).unwrap();
    assert_eq!(read, format!("{large}one more line\n"));
    ok(alt(&origin, &["fsck"]));
}
