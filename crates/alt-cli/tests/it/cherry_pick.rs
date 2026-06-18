//! `alt cherry-pick <rev>`: apply one commit's changes on top of HEAD,
//! recording a new commit with the original author + a "(cherry
//! picked from commit …)" trailer. Conflicts stop and exit 1, same
//! as git.

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

/// Two-branch fixture: main has `a = aa`; feature has `b = b\n`. The
/// returned tuple is `(feature_commit_oid, head_oid_at_main)`.
fn diverged(tmp: &Path) -> (String, String) {
    ok(alt(tmp, &["init"]));
    fs::write(tmp.join("a"), "a\n").unwrap();
    ok(alt(tmp, &["add", "a"]));
    ok(alt(tmp, &["commit", "-m", "c1"]));

    ok(alt(tmp, &["switch", "-c", "feature"]));
    fs::write(tmp.join("b"), "b\n").unwrap();
    ok(alt(tmp, &["add", "b"]));
    ok(alt(tmp, &["commit", "-m", "feature adds b"]));
    let feature_oid = ok(alt(tmp, &["rev-parse", "HEAD"])).trim().to_string();

    ok(alt(tmp, &["switch", "main"]));
    fs::write(tmp.join("a"), "aa\n").unwrap();
    ok(alt(tmp, &["add", "a"]));
    ok(alt(tmp, &["commit", "-m", "main moves a"]));
    let main_oid = ok(alt(tmp, &["rev-parse", "HEAD"])).trim().to_string();

    (feature_oid, main_oid)
}

#[test]
fn cherry_pick_non_conflicting_change_applies_on_top_of_head() {
    let tmp = tempfile::tempdir().unwrap();
    let (feature_oid, main_before) = diverged(tmp.path());

    let out = ok(alt(tmp.path(), &["cherry-pick", &feature_oid]));
    assert!(out.contains("feature adds b"), "got: {out}");

    let head = ok(alt(tmp.path(), &["rev-parse", "HEAD"]))
        .trim()
        .to_string();
    assert_ne!(head, main_before, "HEAD should have moved forward");

    // Working tree carries both files now.
    assert_eq!(
        fs::read_to_string(tmp.path().join("a")).unwrap(),
        "aa\n",
        "a unchanged from main",
    );
    assert_eq!(
        fs::read_to_string(tmp.path().join("b")).unwrap(),
        "b\n",
        "b applied from feature",
    );

    // The trailer is in the message.
    let show = ok(alt(tmp.path(), &["show", "HEAD"]));
    assert!(
        show.contains(&format!("(cherry picked from commit {feature_oid})")),
        "trailer missing: {show}",
    );
}

#[test]
fn cherry_pick_records_only_one_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let (feature_oid, main_before) = diverged(tmp.path());
    ok(alt(tmp.path(), &["cherry-pick", &feature_oid]));
    let head = ok(alt(tmp.path(), &["rev-parse", "HEAD"]));
    let body = ok(alt(tmp.path(), &["cat-file", "-p", head.trim()]));
    let parents: Vec<&str> = body.lines().filter(|l| l.starts_with("parent ")).collect();
    assert_eq!(parents.len(), 1, "exactly one parent: {body}");
    assert!(parents[0].contains(&main_before));
}

#[test]
fn cherry_pick_conflict_returns_exit_code_one() {
    // Both branches edit the same line of the same file → conflict.
    let tmp = tempfile::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    fs::write(tmp.path().join("x"), "one\n").unwrap();
    ok(alt(tmp.path(), &["add", "x"]));
    ok(alt(tmp.path(), &["commit", "-m", "c1"]));

    ok(alt(tmp.path(), &["switch", "-c", "feature"]));
    fs::write(tmp.path().join("x"), "feature\n").unwrap();
    ok(alt(tmp.path(), &["add", "x"]));
    ok(alt(tmp.path(), &["commit", "-m", "feature edits x"]));
    let feature_oid = ok(alt(tmp.path(), &["rev-parse", "HEAD"]))
        .trim()
        .to_string();

    ok(alt(tmp.path(), &["switch", "main"]));
    fs::write(tmp.path().join("x"), "main\n").unwrap();
    ok(alt(tmp.path(), &["add", "x"]));
    ok(alt(tmp.path(), &["commit", "-m", "main edits x"]));

    let out = alt(tmp.path(), &["cherry-pick", &feature_oid]);
    assert_eq!(out.status.code(), Some(1), "expected exit code 1");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("CONFLICT"), "got: {stdout}");
}

#[test]
fn cherry_pick_unknown_rev_fails() {
    let tmp = tempfile::tempdir().unwrap();
    diverged(tmp.path());
    let out = alt(tmp.path(), &["cherry-pick", "nonexistent"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("bad revision"), "got: {err}");
}

#[test]
fn cherry_pick_root_commit_fails() {
    let tmp = tempfile::tempdir().unwrap();
    diverged(tmp.path());
    // The root commit on main is `c1` (HEAD~ from main moves a).
    let root_oid = ok(alt(tmp.path(), &["rev-parse", "main~"]))
        .trim()
        .to_string();
    let out = alt(tmp.path(), &["cherry-pick", &root_oid]);
    assert!(!out.status.success(), "root cherry-pick should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("root commit"), "got: {err}");
}
