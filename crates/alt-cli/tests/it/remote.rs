//! `alt remote add/list/remove`: register git remotes in
//! `<alt-dir>/remotes/<name>` as a minimal key=value text file. This is
//! M6/W3 — the persistence + CLI surface that W4 (`alt fetch`) wires the
//! transport into.

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

/// Adding a remote writes `<alt-dir>/remotes/<name>` with a stable
/// `url=` + `fetch=` body (zero-serde, human-editable); the default
/// fetch refspec routes the remote's branches into `refs/remotes/<name>/*`.
#[test]
fn remote_add_persists_to_disk_with_default_fetch_refspec() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    ok(alt(
        root,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/user/repo.git",
        ],
    ));
    let body = std::fs::read_to_string(root.join(".alt/remotes/origin")).unwrap();
    assert!(
        body.contains("url=https://github.com/user/repo.git"),
        "{body}"
    );
    assert!(
        body.contains("fetch=+refs/heads/*:refs/remotes/origin/*"),
        "{body}"
    );
}

/// `remote list` emits `<name>\t<url>` per line, alphabetical, matching
/// what's on disk. With `--json`, the parallel schema agents consume.
#[test]
fn remote_list_human_and_json_match_persisted_state() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    ok(alt(
        root,
        &[
            "remote",
            "add",
            "upstream",
            "https://example.com/upstream.git",
        ],
    ));
    ok(alt(
        root,
        &["remote", "add", "origin", "https://example.com/origin.git"],
    ));

    let human = ok(alt(root, &["remote", "list"]));
    // alphabetical (origin before upstream)
    let lines: Vec<&str> = human.lines().collect();
    assert_eq!(lines.len(), 2, "{human}");
    assert!(lines[0].starts_with("origin\t"), "{human}");
    assert!(lines[1].starts_with("upstream\t"), "{human}");
    assert!(lines[0].ends_with("/origin.git"), "{human}");
    assert!(lines[1].ends_with("/upstream.git"), "{human}");

    let json = ok(alt(root, &["remote", "list", "--json"]));
    assert!(json.contains("\"schema_version\":1"), "{json}");
    assert!(
        json.contains("\"name\":\"origin\""),
        "origin missing: {json}"
    );
    assert!(
        json.contains("\"name\":\"upstream\""),
        "upstream missing: {json}"
    );
    assert!(
        json.contains("\"fetch\":\"+refs/heads/*:refs/remotes/origin/*\""),
        "fetch refspec missing: {json}"
    );
}

/// Listing on a freshly-init'd repo (no `remotes/` dir) is empty — not
/// an error. JSON path returns an empty array.
#[test]
fn remote_list_on_fresh_repo_is_empty_not_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    assert!(ok(alt(root, &["remote", "list"])).is_empty());
    let json = ok(alt(root, &["remote", "list", "--json"]));
    assert!(json.contains("\"remotes\":[]"), "{json}");
}

/// Adding a duplicate is rejected — `alt remote add origin <new-url>`
/// should not silently rewire an existing remote.
#[test]
fn remote_add_rejects_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    ok(alt(
        root,
        &["remote", "add", "origin", "https://a.example/repo.git"],
    ));
    let out = alt(
        root,
        &["remote", "add", "origin", "https://b.example/repo.git"],
    );
    assert!(!out.status.success(), "duplicate should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("already exists"), "{err}");
    // and the original url is preserved on disk
    let body = std::fs::read_to_string(root.join(".alt/remotes/origin")).unwrap();
    assert!(body.contains("https://a.example/repo.git"), "{body}");
}

/// Removing drops the file but doesn't touch `refs/remotes/<name>/*` —
/// removing the config doesn't pretend ingested history never happened.
#[test]
fn remote_remove_drops_config_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    ok(alt(
        root,
        &["remote", "add", "origin", "https://example.com/repo.git"],
    ));
    assert!(root.join(".alt/remotes/origin").exists());
    ok(alt(root, &["remote", "remove", "origin"]));
    assert!(!root.join(".alt/remotes/origin").exists());

    // removing again is an error (clear failure, not silently fine)
    let out = alt(root, &["remote", "remove", "origin"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no such remote"));
}

/// Invalid remote names are rejected before any file is written —
/// `..`, paths, control chars all fail the name check at the entry
/// point so a hostile name can't escape the `<alt-dir>/remotes/` dir.
#[test]
fn invalid_remote_names_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    for bad in ["..", ".hidden", "with/slash", "with space", ""] {
        let out = alt(root, &["remote", "add", bad, "https://e/x"]);
        assert!(
            !out.status.success(),
            "should reject {bad:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
