//! `alt show <rev>`: a thin wrapper over `alt log -p -n 1 --pretty=raw`
//! that gives users the "show this one commit + its patch" verb every
//! git user expects.

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

fn setup(tmp: &Path) {
    ok(alt(tmp, &["init"]));
    fs::write(tmp.join("readme.txt"), "first\n").unwrap();
    ok(alt(tmp, &["add", "readme.txt"]));
    ok(alt(tmp, &["commit", "-m", "first commit"]));
    fs::write(tmp.join("readme.txt"), "second\n").unwrap();
    ok(alt(tmp, &["add", "readme.txt"]));
    ok(alt(tmp, &["commit", "-m", "second commit"]));
}

#[test]
fn show_head_emits_header_and_unified_diff() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    let out = ok(alt(tmp.path(), &["show", "HEAD"]));
    // header
    assert!(out.starts_with("commit "), "got: {out}");
    assert!(out.contains("author tester <t@e>"), "got: {out}");
    assert!(out.contains("    second commit"), "got: {out}");
    // unified diff: previous content removed, new added
    assert!(out.contains("-first"), "got: {out}");
    assert!(out.contains("+second"), "got: {out}");
}

#[test]
fn show_default_argument_is_head() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    let with = ok(alt(tmp.path(), &["show", "HEAD"]));
    let without = ok(alt(tmp.path(), &["show"]));
    assert_eq!(with, without, "alt show defaults to HEAD");
}

#[test]
fn show_emits_a_single_commit() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    let out = ok(alt(tmp.path(), &["show", "HEAD"]));
    // Only one "commit " header line — the wrapper caps -n 1.
    assert_eq!(
        out.lines().filter(|l| l.starts_with("commit ")).count(),
        1,
        "got: {out}",
    );
}

#[test]
fn show_resolves_tilde_ancestor() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    let head = ok(alt(tmp.path(), &["rev-parse", "HEAD"]));
    let parent = ok(alt(tmp.path(), &["rev-parse", "HEAD~"]));
    assert_ne!(head.trim(), parent.trim(), "HEAD~ != HEAD");
    let show_parent = ok(alt(tmp.path(), &["show", "HEAD~"]));
    assert!(
        show_parent.contains(parent.trim()),
        "expected parent oid in show output\nparent={parent}\nshow={show_parent}",
    );
    assert!(show_parent.contains("first commit"));
}

#[test]
fn rev_parse_supports_tilde_and_caret() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    let by_tilde = ok(alt(tmp.path(), &["rev-parse", "HEAD~1"]));
    let by_caret = ok(alt(tmp.path(), &["rev-parse", "HEAD^"]));
    assert_eq!(by_tilde.trim(), by_caret.trim(), "HEAD~1 == HEAD^");
}

#[test]
fn rev_parse_tilde_past_root_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    // Two commits; HEAD~3 walks past the root.
    let out = alt(tmp.path(), &["rev-parse", "HEAD~3"]);
    assert!(
        !out.status.success(),
        "expected failure when stepping past the root, got: {:?}",
        String::from_utf8_lossy(&out.stdout),
    );
}

#[test]
fn show_json_returns_a_single_commit_object() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    let out = ok(alt(tmp.path(), &["show", "HEAD", "--json"]));
    // Don't deserialise structurally; the field shape is shared with
    // `alt log --json -n 1` and that's covered by its own tests. We
    // just confirm json mode produced JSON-looking output and that the
    // count is one.
    assert!(out.contains("commits"), "got: {out}");
    let opens = out.matches("\"oid\"").count();
    assert_eq!(opens, 1, "expected one commit object, got: {out}");
}
