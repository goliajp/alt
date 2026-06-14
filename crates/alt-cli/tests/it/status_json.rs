//! `alt status --json`: the structured view (VISION §4 A1). The output is
//! valid JSON (round-tripped through python's `json.load`) and carries the
//! stable schema — staged/unstaged/untracked/unmerged plus a `clean` flag.

use std::path::Path;
use std::process::{Command, Output, Stdio};

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

/// Feeds `json` to `python3 -c json.load`; asserts it parses as valid JSON.
fn assert_valid_json(json: &str) {
    let mut child = Command::new("python3")
        .args(["-c", "import json,sys; json.load(sys.stdin)"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "not valid JSON: {json}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn clean_tree_reports_clean_with_empty_sections() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));

    let json = ok(alt(root, &["status", "--json"]));
    assert_valid_json(&json);
    assert!(json.contains("\"schema_version\":1"), "{json}");
    assert!(json.contains("\"branch\":\"main\""), "{json}");
    assert!(json.contains("\"clean\":true"), "{json}");
    assert!(json.contains("\"staged\":[]"), "{json}");
    assert!(json.contains("\"unstaged\":[]"), "{json}");
    assert!(json.contains("\"untracked\":[]"), "{json}");
    assert!(json.contains("\"unmerged\":[]"), "{json}");
}

#[test]
fn dirty_tree_classifies_staged_unstaged_untracked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("tracked.txt"), "v1\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));

    // staged add, unstaged modification of a committed file, an untracked file
    std::fs::write(root.join("staged.txt"), "new\n").unwrap();
    ok(alt(root, &["add", "staged.txt"]));
    std::fs::write(root.join("tracked.txt"), "v2\n").unwrap();
    std::fs::write(root.join("untracked.txt"), "loose\n").unwrap();

    let json = ok(alt(root, &["status", "--json"]));
    assert_valid_json(&json);
    assert!(json.contains("\"clean\":false"), "{json}");
    assert!(
        json.contains("{\"path\":\"staged.txt\",\"change\":\"added\"}"),
        "staged add: {json}"
    );
    assert!(
        json.contains("{\"path\":\"tracked.txt\",\"change\":\"modified\"}"),
        "unstaged modify: {json}"
    );
    assert!(
        json.contains("\"untracked\":[\"untracked.txt\"]"),
        "untracked: {json}"
    );
}

#[test]
fn conflict_surfaces_unmerged_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f.txt"), "line1\nline2\nline3\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    ok(alt(root, &["branch", "feat"]));
    std::fs::write(root.join("f.txt"), "line1\nOURS\nline3\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "ours"]));
    ok(alt(root, &["switch", "feat"]));
    std::fs::write(root.join("f.txt"), "line1\nTHEIRS\nline3\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "theirs"]));
    ok(alt(root, &["switch", "main"]));

    // the merge conflicts (non-zero exit); status --json must list the path
    let m = alt(root, &["merge", "feat"]);
    assert!(!m.status.success(), "conflicting merge must fail");

    let json = ok(alt(root, &["status", "--json"]));
    assert_valid_json(&json);
    assert!(json.contains("\"clean\":false"), "{json}");
    assert!(json.contains("\"unmerged\":[\"f.txt\"]"), "{json}");
}
