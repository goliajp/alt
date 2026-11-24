//! The users file of a root of repositories: a rule in it that does not
//! read is an error of the file, and lets nobody in.

use std::fs;
use std::process::{Command, Stdio};

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_untrusted::repository;

fn line(user: &str, token: &str, rules: &str) -> String {
    let hash = alt_hash::blake3::hash(token.as_bytes()).to_hex();
    format!("{user}\t{hash}\t{rules}\n")
}

/// `docs:x` is no rule. A server is not started with it: left out, `*:rw`
/// would decide for `docs`. One that is running when the file comes to
/// have it answers nobody the file names, whatever their own rules are,
/// until the file reads again.
#[test]
fn a_rule_that_does_not_read_ends_the_start_and_lets_nobody_in_afterwards() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    repository(&root.join("docs"));
    let users = root.join("users");
    let (good, bad) = (
        line("alice", "hers", "*:r"),
        line("bob", "his", "docs:x *:rw"),
    );

    fs::write(&users, format!("{good}{bad}")).unwrap();
    let start = Command::new(env!("CARGO_BIN_EXE_altd-server"))
        .args(["--bind", "127.0.0.1:0"])
        .env("ALT_SERVER_ROOT", &root)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let out = start.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{said}");
    assert!(
        said.contains("users: line 2: \"docs:x\" is no rule"),
        "{said}"
    );
    assert!(!said.contains("listening on"), "{said}");

    fs::write(
        &users,
        format!("{good}{}", line("bob", "his", "docs:n *:rw")),
    )
    .unwrap();
    let server = spawn_server_with_env(&[("ALT_SERVER_ROOT", root.to_str().unwrap())]);
    let told = |user: &str, token: &str, service: &str| {
        let url = format!("http://{}/docs/info/refs?service={service}", server.addr);
        let out = Command::new("curl")
            .args(["-sS", "-o", "/dev/null", "-w", "%{http_code}", "-u"])
            .args([&format!("{user}:{token}"), &url])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert_eq!(told("alice", "hers", "git-upload-pack"), "200");
    assert_eq!(told("bob", "his", "git-upload-pack"), "403");
    fs::write(&users, format!("{good}{bad}")).unwrap();
    for (user, token) in [("alice", "hers"), ("bob", "his"), ("nobody", "none")] {
        for service in ["git-upload-pack", "git-receive-pack"] {
            assert_eq!(told(user, token, service), "500", "{user} {service}");
        }
    }
    fs::write(&users, &good).unwrap();
    assert_eq!(told("alice", "hers", "git-upload-pack"), "200");
}
