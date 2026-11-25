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

/// A server told the peers of its host and not its proxies says, as it
/// starts, that a request through a proxy among those peers counts for no
/// address; one told both says nothing of it.
#[test]
fn a_server_told_its_local_peers_and_not_its_proxies_says_so_as_it_starts() {
    use std::io::{BufRead, BufReader};
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let started = |told: &[(&str, &str)]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_altd-server"));
        command.args(["--bind", "127.0.0.1:0"]);
        command.env("ALT_SERVER_REPO", &origin);
        command.envs(told.iter().copied());
        let mut child = (command.stdout(Stdio::null()).stderr(Stdio::piped()))
            .spawn()
            .unwrap();
        let lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let said: Vec<String> = lines
            .map(Result::unwrap)
            .take_while(|line| !line.contains("listening on"))
            .collect();
        child.kill().unwrap();
        child.wait().unwrap();
        said.join("\n")
    };
    let peers = ("ALT_SERVER_LOCAL_PEERS", "127.0.0.0/8,172.30.90.1");
    let unproxied = "ALT_SERVER_LOCAL_PEERS is set and ALT_SERVER_TRUSTED_PROXIES is not";
    let said = started(&[peers]);
    assert!(said.contains(unproxied), "{said}");
    let said = started(&[peers, ("ALT_SERVER_TRUSTED_PROXIES", "172.30.90.1")]);
    assert!(!said.contains(unproxied), "{said}");
}
