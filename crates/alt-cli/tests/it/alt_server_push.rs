//! `alt push` to alt-server of refs it partly or wholly has already.

use std::path::Path;

use crate::task_remote::Server;

fn alt(root: &Path, token: &str, args: &[&str]) -> String {
    let output = crate::alt_command()
        .current_dir(root)
        .args(args)
        .env("ALT_NO_DAEMON", "1")
        .env("ALT_HTTP_USER_ORIGIN", "tester")
        .env("ALT_HTTP_TOKEN_ORIGIN", token)
        .env("USER", "remote-test")
        .env("GIT_AUTHOR_NAME", "remote-test")
        .env("GIT_AUTHOR_EMAIL", "remote@test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn tip(root: &Path, token: &str, rev: &str) -> String {
    let log = alt(root, token, &["log", "--pretty=oneline", "-n", "1", rev]);
    log.split_whitespace().next().unwrap().to_owned()
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by closing gate"]
fn a_ref_the_server_has_is_reported_and_the_other_created() {
    let server = Server::new();
    let token = server.token.as_str();
    let local = alt_os::temp::tempdir().unwrap();
    let root = local.path();
    alt(root, token, &["init", "."]);
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    alt(root, token, &["add", "."]);
    alt(root, token, &["commit", "-m", "first"]);
    alt(root, token, &["branch", "older"]);
    std::fs::write(root.join("a.txt"), "hello v2\n").unwrap();
    alt(root, token, &["add", "a.txt"]);
    alt(root, token, &["commit", "-m", "second"]);
    let url = format!("{}/git/repository", server.url);
    alt(root, token, &["remote", "add", "origin", &url]);
    alt(root, token, &["push", "origin", "main"]);

    let stdout = alt(root, token, &["push", "origin", "main", "older", "--json"]);
    assert!(
        stdout.contains(concat!(
            r#""objects":0,"operation_ids":[],"pack_bytes":32,"#,
            r#""refs":[{"name":"refs/heads/older","status":"ok"},"#,
            r#"{"name":"refs/heads/main","status":"up-to-date"}],"#
        )),
        "{stdout}"
    );
    assert!(stdout.contains(r#""updated":true"#), "{stdout}");
    let stdout = alt(root, token, &["push", "origin", "main", "older", "--json"]);
    assert!(
        stdout.contains(concat!(
            r#""objects":0,"operation_ids":[],"pack_bytes":0,"#,
            r#""refs":[{"name":"refs/heads/main","status":"up-to-date"},"#,
            r#"{"name":"refs/heads/older","status":"up-to-date"}],"#
        )),
        "{stdout}"
    );
    assert!(stdout.contains(r#""updated":false"#), "{stdout}");

    let copy = alt_os::temp::tempdir().unwrap();
    alt(copy.path(), token, &["init", "."]);
    alt(copy.path(), token, &["remote", "add", "origin", &url]);
    alt(copy.path(), token, &["fetch", "origin"]);
    for branch in ["main", "older"] {
        assert_eq!(
            tip(copy.path(), token, &format!("refs/remotes/origin/{branch}")),
            tip(root, token, &format!("refs/heads/{branch}")),
            "{branch}"
        );
    }
}
