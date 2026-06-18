//! `[url "X"] insteadOf = Y` rewriting from the repo's git-import
//! config. Lets users keep SSH-style URLs (`git@host:user/repo`) on
//! disk while alt's HTTPS-only transport sees the rewritten form.

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
fn rewrites_apply_during_fetch_when_url_starts_with_insteadof_trigger() {
    // The destination of the rewrite is some host that doesn't really
    // exist; we just want to confirm the URL we end up trying to hit
    // is the rewritten one, not the SSH original. alt-wire-http
    // surfaces the URL it tried in its error chain.
    let tmp = tempfile::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    let cfg_dir = tmp.path().join(".alt").join("git-import");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config"),
        "[url \"http://127.0.0.1:1/\"]\n\tinsteadOf = git@host:\n",
    )
    .unwrap();
    ok(alt(
        tmp.path(),
        &["remote", "add", "origin", "git@host:user/repo.git"],
    ));

    let out = alt(tmp.path(), &["fetch", "origin"]);
    assert!(!out.status.success(), "expected fetch to fail (dummy host)");
    let err = String::from_utf8_lossy(&out.stderr);
    // The transport should have been hit with the rewritten URL.
    // Even on connection failure we should see the http://127.0.0.1
    // form in the error chain (alt-wire-http carries the URL through).
    assert!(
        err.contains("127.0.0.1") || err.contains("http://"),
        "expected rewritten URL in error, got: {err}",
    );
    assert!(
        !err.contains("Bad URL"),
        "SSH URL should have been rewritten before parsing: {err}",
    );
}

#[test]
fn builtin_ssh_to_https_rewrites_well_known_hosts() {
    // No insteadOf rule configured — the built-in fallback should
    // still rewrite `git@github.com:foo/bar.git` to HTTPS.
    let tmp = tempfile::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    ok(alt(
        tmp.path(),
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:nope/nonexistent.git",
        ],
    ));

    let out = alt(tmp.path(), &["fetch", "origin"]);
    // The fetch can't actually succeed (private/empty repo), but the
    // error must show alt tried HTTPS — not a "Bad URL" rejection
    // from the SSH form.
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("Bad URL"),
        "SSH URL should be auto-rewritten: {err}",
    );
    assert!(
        err.contains("github.com") || err.contains("http"),
        "expected to hit github over HTTPS: {err}",
    );
}

#[test]
fn explicit_insteadof_wins_over_builtin_fallback() {
    // When the user writes their own insteadOf, the built-in
    // SSH→HTTPS mapping must not override it.
    let tmp = tempfile::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    let cfg_dir = tmp.path().join(".alt").join("git-import");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config"),
        "[url \"http://my-mirror/\"]\n\tinsteadOf = git@github.com:\n",
    )
    .unwrap();
    ok(alt(
        tmp.path(),
        &["remote", "add", "origin", "git@github.com:user/repo.git"],
    ));

    let out = alt(tmp.path(), &["fetch", "origin"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("my-mirror"),
        "expected explicit insteadOf to win, got: {err}",
    );
    assert!(
        !err.contains("github.com"),
        "built-in fallback should not also fire: {err}",
    );
}

#[test]
fn longest_matching_insteadof_wins() {
    // Two insteadOf rules pointing at different replacements; the
    // more specific one (`git@github.com:`) is longer than the
    // catch-all (`git@`) and must win.
    let tmp = tempfile::tempdir().unwrap();
    ok(alt(tmp.path(), &["init"]));
    let cfg_dir = tmp.path().join(".alt").join("git-import");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config"),
        "[url \"http://catch-all/\"]\n\tinsteadOf = git@\n\
         [url \"http://specific/\"]\n\tinsteadOf = git@github.com:\n",
    )
    .unwrap();
    ok(alt(
        tmp.path(),
        &["remote", "add", "gh", "git@github.com:user/repo.git"],
    ));

    let out = alt(tmp.path(), &["fetch", "gh"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("specific"),
        "expected longest-prefix rule to win, got: {err}",
    );
    assert!(
        !err.contains("catch-all"),
        "catch-all rule should not match, got: {err}",
    );
}
