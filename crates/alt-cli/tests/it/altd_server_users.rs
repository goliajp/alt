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
    let mut start = Command::new(env!("CARGO_BIN_EXE_altd-server"))
        .args(["--bind", "127.0.0.1:0"])
        .env("ALT_SERVER_ROOT", &root)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // one that took the file serves, and does not end
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while start.try_wait().unwrap().is_none() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = start.kill();
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
            .args(["-sS", "--max-time", "60", "-o", "/dev/null"])
            .args(["-w", "%{http_code}", "-u"])
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

/// `check-users` reads a users file as the server does and tells the lines an
/// operator is to look at: a user held to no rules, and one whose rules gave
/// more when they were added up. It ends with 1 where there are such lines,
/// 0 where there are none, and 2 for a file the server would not start with.
#[test]
fn the_check_of_a_users_file_tells_the_lines_that_give_all_or_less_than_before() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let users = tmp.path().join("users");
    let checked = |text: &str| {
        fs::write(&users, text).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_altd-server"))
            .arg("check-users")
            .arg(&users)
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout).into_owned();
        (
            out.status.code(),
            said,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let file = [
        line("alice", "a", "docs:rw"),
        "bob\t00\n".into(),
        line("carol", "c", "*:r\tdocs:w"),
    ];
    let (code, said, _) = checked(&file.concat());
    assert_eq!(code, Some(1), "{said}");
    let expected = "line 2: bob: no rules: every repository is read and written\n\
                    line 3: carol: docs was read and written and is written\n";
    assert_eq!(said, expected);
    assert_eq!(checked(&line("alice", "a", "docs:rw *:r")).0, Some(0));
    let (code, said, why) = checked(&line("dave", "d", "docs:x"));
    assert_eq!((code, said.as_str()), (Some(2), ""), "{why}");
    assert!(why.contains("line 1: \"docs:x\" is no rule"), "{why}");
}

/// A server that started with a users file lets nobody in while the file is
/// not there: not one who names no user, as a server without users would, and
/// not one the file named. Put back, it lets in whom it names. A server that
/// started without one serves whoever asks, as before.
#[test]
fn a_users_file_that_is_gone_while_the_server_runs_opens_nothing() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let root = tmp.path().join("root");
    repository(&root.join("docs"));
    let users = root.join("users");
    let told = |addr: &dyn std::fmt::Display, who: Option<&str>| {
        let url = format!("http://{addr}/docs/info/refs?service=git-upload-pack");
        let mut curl = Command::new("curl");
        curl.args(["-sS", "--max-time", "60", "-o", "/dev/null"]);
        curl.args(["-w", "%{http_code}"]);
        if let Some(who) = who {
            curl.args(["-u", who]);
        }
        let out = curl.arg(&url).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let root_env = [("ALT_SERVER_ROOT", root.to_str().unwrap())];
    let open = spawn_server_with_env(&root_env);
    assert_eq!(told(&open.addr, None), "200");
    drop(open);

    fs::write(&users, line("alice", "hers", "*:r")).unwrap();
    let server = spawn_server_with_env(&root_env);
    let alice = Some("alice:hers");
    assert_eq!(told(&server.addr, alice), "200");
    assert_eq!(told(&server.addr, None), "401");
    // taken away, as one who writes it anew in its place does for a moment
    fs::remove_file(&users).unwrap();
    assert_eq!(told(&server.addr, None), "401");
    assert_eq!(told(&server.addr, alice), "500");
    assert_eq!(told(&server.addr, Some("mallory:any")), "500");
    // a directory in its place is no users file either
    fs::create_dir(&users).unwrap();
    assert_eq!(told(&server.addr, None), "401");
    assert_eq!(told(&server.addr, alice), "500");
    fs::remove_dir(&users).unwrap();
    fs::write(&users, line("alice", "hers", "*:r")).unwrap();
    assert_eq!(told(&server.addr, alice), "200");
    assert_eq!(told(&server.addr, None), "401");
}

/// A users file that is a link to nothing, or a directory, is a users file
/// that does not read: the server is not started, where it was started for
/// anybody to read and write.
#[test]
fn a_users_file_that_is_a_link_to_nothing_ends_the_start() {
    for made in ["link", "directory"] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let root = tmp.path().join("root");
        repository(&root.join("docs"));
        let users = root.join("users");
        match made {
            "link" => std::os::unix::fs::symlink(tmp.path().join("gone"), &users).unwrap(),
            _ => fs::create_dir(&users).unwrap(),
        }
        let mut start = Command::new(env!("CARGO_BIN_EXE_altd-server"))
            .args(["--bind", "127.0.0.1:0"])
            .env("ALT_SERVER_ROOT", &root)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while start.try_wait().unwrap().is_none() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = start.kill();
        let out = start.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{made}: {said}");
        assert!(said.contains("users"), "{made}: {said}");
        assert!(!said.contains("listening on"), "{made}: {said}");
    }
}
