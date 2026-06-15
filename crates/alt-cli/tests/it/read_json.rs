//! `--json` on the read commands `diff` / `branch` / `log` (VISION §4 A1).
//! Each emits a stable schema that round-trips through python's `json.load`;
//! the assertions pin the fields agents rely on.

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
fn diff_json_carries_structured_hunks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f.txt"), "a\nb\nc\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    // unstaged edit: index → working tree diff
    std::fs::write(root.join("f.txt"), "a\nB\nc\n").unwrap();

    let json = ok(alt(root, &["diff", "--json"]));
    assert_valid_json(&json);
    assert!(json.contains("\"schema_version\":1"), "{json}");
    assert!(json.contains("\"path\":\"f.txt\""), "{json}");
    assert!(json.contains("\"status\":\"modified\""), "{json}");
    assert!(json.contains("\"binary\":false"), "{json}");
    assert!(json.contains("\"old_mode\":\"100644\""), "{json}");
    // the one changed line shows up as a remove + an add inside a hunk
    assert!(
        json.contains("{\"tag\":\"remove\",\"content\":\"b\\n\"}"),
        "{json}"
    );
    assert!(
        json.contains("{\"tag\":\"add\",\"content\":\"B\\n\"}"),
        "{json}"
    );
    assert!(json.contains("\"old_start\":1"), "{json}");
}

#[test]
fn diff_json_flags_binary_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("data.bin"), b"\x00\x01\x02first\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    std::fs::write(root.join("data.bin"), b"\x00\x01\x02second\n").unwrap();

    let json = ok(alt(root, &["diff", "--json"]));
    assert_valid_json(&json);
    assert!(json.contains("\"binary\":true"), "{json}");
    // binary files carry no hunks
    assert!(json.contains("\"hunks\":[]"), "{json}");
    // E2: binary files now carry an A8 B1 chunk-diff summary in the JSON
    // surface — `kind: "binary_chunk_diff"` plus the counts/ratio. Text
    // files still report `chunk_diff: null` (negative space below).
    assert!(
        json.contains("\"chunk_diff\":{\"kind\":\"binary_chunk_diff\""),
        "binary chunk_diff missing: {json}"
    );
    assert!(
        json.contains("\"byte_shared_ratio\""),
        "byte_shared_ratio missing: {json}"
    );
}

/// E2: text-file entries leave the new `chunk_diff` field as `null` — keeps
/// the v1 schema additive (adding a field, never repurposing one) and gives
/// agents a clean negative-space check.
#[test]
fn diff_json_chunk_diff_is_null_for_text_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "v1\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    std::fs::write(root.join("a.txt"), "v2\n").unwrap();

    let json = ok(alt(root, &["diff", "--json"]));
    assert_valid_json(&json);
    assert!(json.contains("\"binary\":false"), "{json}");
    assert!(
        json.contains("\"chunk_diff\":null"),
        "text file chunk_diff should be null: {json}"
    );
}

#[test]
fn diff_json_cached_shows_added_status() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("new.txt"), "fresh\n").unwrap();
    ok(alt(root, &["add", "."]));

    // HEAD → index: the staged new file is an addition with a null old side
    let json = ok(alt(root, &["diff", "--cached", "--json"]));
    assert_valid_json(&json);
    assert!(json.contains("\"status\":\"added\""), "{json}");
    assert!(json.contains("\"old_oid\":null"), "{json}");
    assert!(json.contains("\"new_mode\":\"100644\""), "{json}");
}

#[test]
fn branch_json_lists_branches_with_current_flag() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "x\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    ok(alt(root, &["branch", "feat"]));

    let json = ok(alt(root, &["branch", "--json"]));
    assert_valid_json(&json);
    assert!(json.contains("\"schema_version\":1"), "{json}");
    assert!(json.contains("\"current\":\"main\""), "{json}");
    assert!(
        json.contains("{\"name\":\"feat\",\"current\":false,"),
        "feat is not current: {json}"
    );
    assert!(
        json.contains("{\"name\":\"main\",\"current\":true,"),
        "main is current: {json}"
    );
    // both branches sit at the same commit (feat just forked main)
    let oid = ok(alt(root, &["log", "--json"]));
    assert_valid_json(&oid);
}

#[test]
fn log_json_carries_commit_fields_and_parents() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));
    std::fs::write(root.join("a.txt"), "two\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "second"]));

    let json = ok(alt(root, &["log", "--json"]));
    assert_valid_json(&json);
    assert!(json.contains("\"schema_version\":1"), "{json}");
    assert!(json.contains("\"message\":\"first\\n\""), "{json}");
    assert!(json.contains("\"message\":\"second\\n\""), "{json}");
    assert!(json.contains("\"author\":\"tester <t@e>"), "{json}");
    // the second commit names the first as its parent; the first has none
    assert!(
        json.contains("\"parents\":[]"),
        "root commit has no parents: {json}"
    );
    assert!(
        json.matches("\"parents\":[\"").count() >= 1,
        "child commit lists a parent: {json}"
    );
}
