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
fn blame_follows_renames_with_dash_m() {
    let tmp = tempfile::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    fs::write(
        tmp.path().join("old.txt"),
        "alpha\nbeta\ngamma\ndelta\nepsilon\n",
    )
    .unwrap();
    ok(alt(tmp.path(), &["add", "old.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "add old"]));
    let c1 = ok(alt(tmp.path(), &["rev-parse", "HEAD"]))
        .trim()
        .to_string();

    // Rename + edit the last line.
    fs::rename(tmp.path().join("old.txt"), tmp.path().join("new.txt")).unwrap();
    fs::write(
        tmp.path().join("new.txt"),
        "alpha\nbeta\ngamma\ndelta\nZETA\n",
    )
    .unwrap();
    // alt doesn't have a single `mv` command yet; the existing rename
    // shows up as an old.txt deletion + new.txt addition in the index.
    // Tell alt about both.
    ok(alt(tmp.path(), &["add", "old.txt"])); // records the delete
    ok(alt(tmp.path(), &["add", "new.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "rename + edit"]));
    let c2 = ok(alt(tmp.path(), &["rev-parse", "HEAD"]))
        .trim()
        .to_string();

    // Without -M, every line is attributed to the rename commit (the
    // walker has no way to find the file in the parent under its old
    // name).
    let plain = ok(alt(tmp.path(), &["blame", "new.txt"]));
    let s2 = &c2[..8];
    let unique: std::collections::HashSet<&str> = plain.lines().map(|l| &l[..8]).collect();
    assert_eq!(unique.len(), 1, "plain blame attributes one commit");
    assert!(unique.contains(s2));

    // With -M, unchanged lines should reach back to c1 under old.txt;
    // the edited line stays at c2.
    let follow = ok(alt(tmp.path(), &["blame", "-M", "new.txt"]));
    let s1 = &c1[..8];
    let lines: Vec<&str> = follow.lines().collect();
    assert_eq!(lines.len(), 5, "five lines");
    assert!(lines[0].starts_with(s1), "alpha → c1: {}", lines[0]);
    assert!(lines[1].starts_with(s1), "beta → c1: {}", lines[1]);
    assert!(lines[2].starts_with(s1), "gamma → c1: {}", lines[2]);
    assert!(lines[3].starts_with(s1), "delta → c1: {}", lines[3]);
    assert!(lines[4].starts_with(s2), "ZETA → c2: {}", lines[4]);
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
