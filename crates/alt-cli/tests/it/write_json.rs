//! `--json` on the write commands `add` / `commit` / `switch` / `merge` /
//! `flow` / `undo` (VISION §4 A1): an agent reads the consequence of each
//! state change (new oids, conflict lists, op effects) as a stable schema.

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
fn add_and_commit_json_report_counts_and_oids() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "x\n").unwrap();
    std::fs::write(root.join("b.txt"), "y\n").unwrap();

    let add = ok(alt(root, &["add", ".", "--json"]));
    assert_valid_json(&add);
    assert!(add.contains("\"staged\":2"), "{add}");

    let commit = ok(alt(root, &["commit", "-m", "first", "--json"]));
    assert_valid_json(&commit);
    assert!(commit.contains("\"branch\":\"main\""), "{commit}");
    // a 40-hex commit and tree oid are present
    assert!(commit.contains("\"commit\":\""), "{commit}");
    assert!(commit.contains("\"tree\":\""), "{commit}");
}

#[test]
fn switch_json_reports_result() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "x\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));

    let created = ok(alt(root, &["switch", "-c", "feat", "--json"]));
    assert_valid_json(&created);
    assert!(created.contains("\"branch\":\"feat\""), "{created}");
    assert!(created.contains("\"result\":\"created\""), "{created}");

    let already = ok(alt(root, &["switch", "feat", "--json"]));
    assert!(already.contains("\"result\":\"already_on\""), "{already}");

    let back = ok(alt(root, &["switch", "main", "--json"]));
    assert!(back.contains("\"result\":\"switched\""), "{back}");
}

#[test]
fn merge_json_clean_and_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f.txt"), "l1\nl2\nl3\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));

    // fast-forward: feat advances, main merges it
    ok(alt(root, &["switch", "-c", "feat"]));
    std::fs::write(root.join("f.txt"), "l1\nl2\nl3\nl4\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "advance"]));
    ok(alt(root, &["switch", "main"]));
    let ff = ok(alt(root, &["merge", "feat", "--json"]));
    assert_valid_json(&ff);
    assert!(ff.contains("\"result\":\"fast_forward\""), "{ff}");
    assert!(ff.contains("\"conflicts\":[]"), "{ff}");

    // conflict: two branches diverge on the same line
    ok(alt(root, &["switch", "-c", "x"]));
    std::fs::write(root.join("f.txt"), "l1\nXXX\nl3\nl4\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "x"]));
    ok(alt(root, &["switch", "main"]));
    std::fs::write(root.join("f.txt"), "l1\nMMM\nl3\nl4\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "m"]));

    let m = alt(root, &["merge", "x", "--json"]);
    assert!(!m.status.success(), "conflicting merge exits non-zero");
    let json = String::from_utf8(m.stdout).unwrap();
    assert_valid_json(&json);
    assert!(json.contains("\"result\":\"conflicted\""), "{json}");
    assert!(json.contains("\"commit\":null"), "{json}");
    assert!(json.contains("\"conflicts\":[\"f.txt\"]"), "{json}");
}

#[test]
fn flow_and_undo_json_report_effects() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "x\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));

    let init = ok(alt(root, &["flow", "init", "--json"]));
    assert_valid_json(&init);
    assert!(init.contains("\"develop\":\"develop\""), "{init}");
    assert!(init.contains("\"already_initialized\":false"), "{init}");

    let start = ok(alt(root, &["flow", "feature", "start", "foo", "--json"]));
    assert_valid_json(&start);
    assert!(start.contains("\"branch\":\"feature/foo\""), "{start}");
    assert!(start.contains("\"base\":\"develop\""), "{start}");

    std::fs::write(root.join("a.txt"), "y\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "work"]));

    let finish = ok(alt(root, &["flow", "feature", "finish", "foo", "--json"]));
    assert_valid_json(&finish);
    assert!(finish.contains("\"target\":\"develop\""), "{finish}");
    assert!(finish.contains("\"deleted\":\"feature/foo\""), "{finish}");

    // undo the finish; the affected refs come back in the JSON result
    let undo = ok(alt(root, &["undo", "--json"]));
    assert_valid_json(&undo);
    assert!(undo.contains("\"undone\":true"), "{undo}");
    assert!(undo.contains("refs/heads/develop"), "{undo}");
}
