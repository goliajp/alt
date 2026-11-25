//! What the environment tells `altd-server`: a number that does not
//! read as one ends the start, and is not taken for the default.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::altd_server_untrusted::repository;

#[test]
fn a_limit_that_is_no_number_ends_the_start_with_its_name() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    for (name, value) in [
        ("ALT_SERVER_MAX_PUSH_BYTES", "100M"),
        ("ALT_SERVER_WORKERS", "0"),
        ("ALT_SERVER_IO_TIMEOUT_MS", "-1"),
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_altd-server"))
            .args(["--bind", "127.0.0.1:0"])
            .env("ALT_SERVER_REPO", &origin)
            .env(name, value)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // one that took the value for a number serves, and does not end
        let until = Instant::now() + Duration::from_secs(60);
        let ended = loop {
            match child.try_wait().unwrap() {
                Some(status) => break Some(status),
                None if Instant::now() >= until => break None,
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        if ended.is_none() {
            child.kill().unwrap();
        }
        let out = child.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&out.stderr);
        let ended = ended.unwrap_or_else(|| panic!("{name}={value} did not end the start: {said}"));
        assert!(!ended.success(), "{said}");
        assert!(said.contains(&format!("{name}={value:?}: ")), "{said}");
        assert!(!said.contains("listening on"), "{said}");
    }
}
