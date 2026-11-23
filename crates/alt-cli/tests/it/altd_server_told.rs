//! What the environment tells `altd-server`: a number that does not
//! read as one ends the start, and is not taken for the default.

use std::process::Command;

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
        let out = Command::new(env!("CARGO_BIN_EXE_altd-server"))
            .args(["--bind", "127.0.0.1:0"])
            .env("ALT_SERVER_REPO", &origin)
            .env(name, value)
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{said}");
        assert!(said.contains(&format!("{name}={value:?}: ")), "{said}");
        assert!(!said.contains("listening on"), "{said}");
    }
}
