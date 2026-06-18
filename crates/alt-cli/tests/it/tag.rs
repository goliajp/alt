//! `alt tag`: list / create-lightweight / delete tags as `refs/tags/<name>`.

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

fn setup(tmp: &Path) -> String {
    ok(alt(tmp, &["init"]));
    fs::write(tmp.join("readme.txt"), "first\n").unwrap();
    ok(alt(tmp, &["add", "readme.txt"]));
    ok(alt(tmp, &["commit", "-m", "first commit"]));
    ok(alt(tmp, &["rev-parse", "HEAD"])).trim().to_string()
}

#[test]
fn tag_create_lightweight_at_head_then_list() {
    let tmp = tempfile::tempdir().unwrap();
    let head = setup(tmp.path());

    let create = ok(alt(tmp.path(), &["tag", "v1.0"]));
    assert!(create.contains(&head), "got: {create}");

    let list = ok(alt(tmp.path(), &["tag"]));
    assert!(list.lines().any(|l| l == "v1.0"), "got: {list}");

    // The tag should resolve through rev-parse.
    let resolved = ok(alt(tmp.path(), &["rev-parse", "v1.0"]))
        .trim()
        .to_string();
    assert_eq!(resolved, head);
}

#[test]
fn tag_create_at_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let c1 = setup(tmp.path());
    fs::write(tmp.path().join("readme.txt"), "second\n").unwrap();
    ok(alt(tmp.path(), &["add", "readme.txt"]));
    ok(alt(tmp.path(), &["commit", "-m", "second commit"]));

    ok(alt(tmp.path(), &["tag", "v0.1", &c1]));
    let resolved = ok(alt(tmp.path(), &["rev-parse", "v0.1"]))
        .trim()
        .to_string();
    assert_eq!(resolved, c1);
}

#[test]
fn tag_delete_removes_the_ref() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    ok(alt(tmp.path(), &["tag", "v1.0"]));
    ok(alt(tmp.path(), &["tag", "-d", "v1.0"]));

    let list = ok(alt(tmp.path(), &["tag"]));
    assert!(
        !list.lines().any(|l| l == "v1.0"),
        "v1.0 should be gone: {list}",
    );
    let bad = alt(tmp.path(), &["rev-parse", "v1.0"]);
    assert!(
        !bad.status.success(),
        "rev-parse of deleted tag should fail",
    );
}

#[test]
fn tag_duplicate_create_fails() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    ok(alt(tmp.path(), &["tag", "v1.0"]));
    let out = alt(tmp.path(), &["tag", "v1.0"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("already exists"), "got stderr: {err}");
}

#[test]
fn tag_list_json_emits_sorted_array() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    ok(alt(tmp.path(), &["tag", "v0.2"]));
    ok(alt(tmp.path(), &["tag", "v0.1"]));

    let json = ok(alt(tmp.path(), &["tag", "--json"]));
    // Don't pull in a json crate just to check shape — sanity-check
    // string ordering instead. v0.1 should appear before v0.2.
    let p1 = json.find("\"v0.1\"").expect("v0.1 present");
    let p2 = json.find("\"v0.2\"").expect("v0.2 present");
    assert!(p1 < p2, "expected v0.1 before v0.2 in: {json}");
}
