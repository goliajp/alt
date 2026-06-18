//! `alt blame <path>`: line-by-line origin attribution by walking
//! first-parent and diffing each commit's file content against its
//! parent's. Lines surviving unchanged inherit the parent's commit as
//! their origin; lines changed at a commit stay attributed to it.

use std::fs;
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
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn blame_single_commit_attributes_every_line_to_that_commit() {
    let tmp = tempfile::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    fs::write(tmp.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
    ok(alt(tmp.path(), &["add", "a.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "first"]));
    let head = ok(alt(tmp.path(), &["rev-parse", "HEAD"]))
        .trim()
        .to_string();
    let short = &head[..8];

    let blame_out = ok(alt(tmp.path(), &["blame", "a.txt"]));
    let lines: Vec<&str> = blame_out.lines().collect();
    assert_eq!(lines.len(), 3, "expected 3 lines: {blame_out}");
    for line in &lines {
        assert!(
            line.starts_with(short),
            "expected origin {short} on every line, got: {line}",
        );
    }
}

#[test]
fn blame_assigns_lines_to_the_commits_that_introduced_them() {
    let tmp = tempfile::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    fs::write(tmp.path().join("f.txt"), "alpha\nbeta\n").unwrap();
    ok(alt(tmp.path(), &["add", "f.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "c1"]));
    let c1 = ok(alt(tmp.path(), &["rev-parse", "HEAD"]))
        .trim()
        .to_string();

    fs::write(tmp.path().join("f.txt"), "alpha\nbeta\ngamma\n").unwrap();
    ok(alt(tmp.path(), &["add", "f.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "c2"]));
    let c2 = ok(alt(tmp.path(), &["rev-parse", "HEAD"]))
        .trim()
        .to_string();

    let blame_out = ok(alt(tmp.path(), &["blame", "f.txt"]));
    let lines: Vec<&str> = blame_out.lines().collect();
    assert_eq!(lines.len(), 3, "got: {blame_out}");

    let s1 = &c1[..8];
    let s2 = &c2[..8];
    assert!(lines[0].starts_with(s1), "alpha → c1: {}", lines[0]);
    assert!(lines[1].starts_with(s1), "beta → c1: {}", lines[1]);
    assert!(lines[2].starts_with(s2), "gamma → c2: {}", lines[2]);
}

#[test]
fn blame_missing_file_fails_clearly() {
    let tmp = tempfile::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    fs::write(tmp.path().join("a.txt"), "x\n").unwrap();
    ok(alt(tmp.path(), &["add", "a.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "first"]));

    let out = alt(tmp.path(), &["blame", "no/such/file.txt"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not found"), "stderr: {err}");
}

#[test]
fn blame_at_revision_uses_that_history_only() {
    // Build a 2-commit history; blame at <c1> must attribute lines to
    // c1 only — never reaches forward to c2's later edits.
    let tmp = tempfile::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    fs::write(tmp.path().join("g.txt"), "a\nb\n").unwrap();
    ok(alt(tmp.path(), &["add", "g.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "c1"]));
    let c1 = ok(alt(tmp.path(), &["rev-parse", "HEAD"]))
        .trim()
        .to_string();
    fs::write(tmp.path().join("g.txt"), "a\nb\nc\n").unwrap();
    ok(alt(tmp.path(), &["add", "g.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "c2"]));

    let at_c1 = ok(alt(tmp.path(), &["blame", "g.txt", &c1]));
    let lines: Vec<&str> = at_c1.lines().collect();
    assert_eq!(lines.len(), 2, "at c1 the file has 2 lines: {at_c1}");
    let s1 = &c1[..8];
    for line in &lines {
        assert!(line.starts_with(s1), "got: {line}");
    }
}
