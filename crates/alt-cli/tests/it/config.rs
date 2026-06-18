//! `alt config <key> | --list`: read-only view of the effective git
//! config the alt repo sees (the `<alt-dir>/git-import/config` file,
//! plus its `include` / `includeIf` resolutions).

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
    // Drop a hand-written config into the alt store; the on-disk path
    // alt-repo reads is `<alt-dir>/git-import/config`, mirroring the
    // import contract. We bypass `alt config <key> <value>` because
    // write support is out of scope for this commit.
    let dir = tmp.join(".alt").join("git-import");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("config"),
        "[user]\n\tname = Tester\n\temail = test@example.com\n[core]\n\tcustom = yes\n",
    )
    .unwrap();
}

#[test]
fn config_reads_a_single_key() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    let name = ok(alt(tmp.path(), &["config", "user.name"]));
    assert_eq!(name.trim(), "Tester");
    let email = ok(alt(tmp.path(), &["config", "user.email"]));
    assert_eq!(email.trim(), "test@example.com");
}

#[test]
fn config_missing_key_fails() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    let out = alt(tmp.path(), &["config", "nope.key"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not set"), "got: {err}");
}

#[test]
fn config_list_emits_every_entry() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    let out = ok(alt(tmp.path(), &["config", "--list"]));
    // expect every key in alphabetical-by-file order (file order, not
    // sorted — the implementation walks `cfg.entries`)
    assert!(out.contains("user.name=Tester"), "got: {out}");
    assert!(out.contains("user.email=test@example.com"), "got: {out}");
    assert!(out.contains("core.custom=yes"), "got: {out}");
}

#[test]
fn config_rejects_malformed_dotted_key() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    let out = alt(tmp.path(), &["config", "user"]); // no dot
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("bad config key"), "got: {err}");
}
