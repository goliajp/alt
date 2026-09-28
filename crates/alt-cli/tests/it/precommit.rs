//! Pre-commit checks: a secret in the lines a commit adds stops it, a large
//! file only warns, `--no-verify` skips them, and `--validate --json` gives
//! agents a machine-readable verdict without committing.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

/// A cloud-style access key id, assembled here so this file holds none.
fn fake_key() -> String {
    format!("{}{}", "AK", "IAQWERTYUIOPASDFGH")
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    ok(alt(dir.path(), &["init", "."]));
    std::fs::write(dir.path().join("readme.txt"), "hi\n").unwrap();
    ok(alt(dir.path(), &["add", "."]));
    ok(alt(dir.path(), &["commit", "-m", "base"]));
    dir
}

#[test]
fn a_staged_secret_stops_the_commit_until_skipped() {
    let dir = repo();
    let root = dir.path();
    let before = ok(alt(root, &["rev-parse", "main"]));
    std::fs::write(
        root.join("config.env"),
        format!("region=eu\nid={}\n", fake_key()),
    )
    .unwrap();
    ok(alt(root, &["add", "."]));

    let o = alt(root, &["commit", "-m", "add config"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("secret-scan/cloud-access-key] config.env:2"),
        "{err}"
    );
    assert!(
        !err.contains(&fake_key()),
        "the key itself must not be echoed: {err}"
    );
    assert_eq!(ok(alt(root, &["rev-parse", "main"])), before);

    ok(alt(
        root,
        &["commit", "--no-verify", "-m", "add config anyway"],
    ));
    assert_ne!(ok(alt(root, &["rev-parse", "main"])), before);
}

#[test]
fn validate_reports_json_without_committing() {
    let dir = repo();
    let root = dir.path();
    std::fs::write(root.join("k.txt"), format!("{}\n", fake_key())).unwrap();
    ok(alt(root, &["add", "."]));
    let o = alt(root, &["commit", "--validate", "--json"]);
    assert_eq!(o.status.code(), Some(1));
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(
        out.starts_with("{\"schema_version\":1,\"ok\":false,\"findings\":[{"),
        "{out}"
    );
    assert!(out.contains("\"rule\":\"cloud-access-key\""), "{out}");
    assert!(out.contains("\"line\":1"), "{out}");

    std::fs::write(root.join("k.txt"), "nothing here\n").unwrap();
    ok(alt(root, &["add", "."]));
    let out = ok(alt(root, &["commit", "--validate", "--json"]));
    assert_eq!(
        out.trim(),
        "{\"schema_version\":1,\"ok\":true,\"findings\":[]}"
    );
    assert_eq!(
        ok(alt(root, &["log", "--pretty=oneline"])).lines().count(),
        1
    );
}

#[test]
fn a_secret_already_committed_is_not_raised_again() {
    let dir = repo();
    let root = dir.path();
    std::fs::write(root.join("old.txt"), format!("{}\n", fake_key())).unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-n", "-m", "legacy"]));
    std::fs::write(
        root.join("old.txt"),
        format!("{}\nanother line\n", fake_key()),
    )
    .unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "touch the file elsewhere"]));
}

#[test]
fn the_allow_marker_exempts_a_line() {
    let dir = repo();
    let root = dir.path();
    std::fs::write(
        root.join("fixture.txt"),
        format!("{} # alt:allow-secret\n", fake_key()),
    )
    .unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "documented example key"]));
}

#[test]
fn a_large_file_warns_but_commits() {
    let dir = repo();
    let root = dir.path();
    std::fs::write(root.join("big.bin"), vec![7u8; 11 * 1024 * 1024]).unwrap();
    ok(alt(root, &["add", "."]));
    let o = alt(root, &["commit", "-m", "big"]);
    assert!(o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("warning [large-file-guard/large-file] big.bin"),
        "{err}"
    );
}

#[test]
fn amend_is_checked_too() {
    let dir = repo();
    let root = dir.path();
    std::fs::write(root.join("readme.txt"), format!("hi\n{}\n", fake_key())).unwrap();
    ok(alt(root, &["add", "."]));
    assert!(
        !alt(root, &["commit", "--amend", "--no-edit"])
            .status
            .success()
    );
    ok(alt(
        root,
        &["commit", "--amend", "--no-edit", "--no-verify"],
    ));
}
