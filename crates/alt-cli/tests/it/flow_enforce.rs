//! alt enforces git-flow as *the* collaboration model: commits to
//! `main` or `develop` are refused, with a clear pointer at the
//! escape paths. The hard-rule is what gives AI agents a stable
//! contract — pick a topic branch, work there.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_alt"));
    cmd.current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn bootstrap(tmp: &Path) {
    ok(alt(tmp, &["init"], &[]));
    fs::write(tmp.join("a.txt"), "alpha\n").unwrap();
    ok(alt(tmp, &["add", "a.txt"], &[]));
    // The very first commit on an unborn `main` is allowed: there's
    // nothing to branch off yet.
    ok(alt(tmp, &["commit", "-m", "initial"], &[]));
    ok(alt(tmp, &["flow", "init"], &[]));
}

#[test]
fn commit_on_develop_is_refused_with_escape_paths_in_the_error() {
    let tmp = tempfile::tempdir().unwrap();
    bootstrap(tmp.path());

    fs::write(tmp.path().join("b.txt"), "beta\n").unwrap();
    ok(alt(tmp.path(), &["add", "b.txt"], &[]));

    let out = alt(tmp.path(), &["commit", "-m", "b"], &[]);
    assert!(!out.status.success(), "commit on develop should be refused");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("refusing to commit"), "got: {err}");
    assert!(err.contains("develop"), "got: {err}");
    // Both escape paths must be visible.
    assert!(
        err.contains("alt switch -c feature/"),
        "missing switch path: {err}",
    );
    assert!(
        err.contains("alt flow feature start"),
        "missing flow path: {err}",
    );
}

#[test]
fn commit_on_main_suggests_hotfix_path() {
    let tmp = tempfile::tempdir().unwrap();
    bootstrap(tmp.path());
    // Bounce back to main; flow init left us on develop.
    ok(alt(tmp.path(), &["switch", "main"], &[]));
    fs::write(tmp.path().join("c.txt"), "gamma\n").unwrap();
    ok(alt(tmp.path(), &["add", "c.txt"], &[]));

    let out = alt(tmp.path(), &["commit", "-m", "c"], &[]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("hotfix"), "main → hotfix path: {err}");
}

#[test]
fn commit_on_feature_branch_passes() {
    let tmp = tempfile::tempdir().unwrap();
    bootstrap(tmp.path());

    // Stage on develop, then switch -c into a feature branch carrying
    // the changes — this is the escape path the error message shows.
    fs::write(tmp.path().join("d.txt"), "delta\n").unwrap();
    ok(alt(tmp.path(), &["add", "d.txt"], &[]));
    ok(alt(tmp.path(), &["switch", "-c", "feature/my-work"], &[]));

    let out = ok(alt(tmp.path(), &["commit", "-m", "d on feature"], &[]));
    assert!(out.contains("[feature/my-work]"), "got: {out}");
}

#[test]
fn alt_protected_override_unlocks_the_guard() {
    let tmp = tempfile::tempdir().unwrap();
    bootstrap(tmp.path());
    fs::write(tmp.path().join("e.txt"), "epsilon\n").unwrap();
    ok(alt(tmp.path(), &["add", "e.txt"], &[]));

    // With the env var set, the guard short-circuits and the commit
    // goes through on develop. This exists for emergencies (recovery,
    // CI tooling work, …) — not for normal use.
    let out = ok(alt(
        tmp.path(),
        &["commit", "-m", "emergency on develop"],
        &[("ALT_PROTECTED_OVERRIDE", "1")],
    ));
    assert!(out.contains("[develop]"), "got: {out}");
}

#[test]
fn unborn_main_initial_commit_is_allowed() {
    // The very first `alt commit` after `alt init` lands on an unborn
    // `main` — the guard must not block it. This is what bootstrap()
    // relies on in every test above.
    let tmp = tempfile::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"], &[]));
    fs::write(tmp.path().join("a.txt"), "first\n").unwrap();
    ok(alt(tmp.path(), &["add", "a.txt"], &[]));
    let out = ok(alt(tmp.path(), &["commit", "-m", "first"], &[]));
    assert!(out.contains("[main]"), "got: {out}");
}
