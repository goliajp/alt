//! `alt diff`: unstaged (index → work tree) and `--cached` (HEAD → index),
//! with binary detection. The hunk body is cross-checked against real git.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

/// The unified body (from the first `@@` on) of a diff, so we can compare our
/// output to git's without coupling to the exact header bytes.
fn hunk_body(diff: &str) -> String {
    match diff.find("@@") {
        Some(i) => diff[i..].to_string(),
        None => String::new(),
    }
}

#[test]
fn diff_unstaged_and_cached_match_git_hunks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f.txt"), "line1\nline2\nline3\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));

    // a clean tree has no diff
    assert!(ok(alt(root, &["diff"])).is_empty(), "clean tree => no diff");

    // edit the file in the working tree; `alt diff` shows the unstaged change
    std::fs::write(root.join("f.txt"), "line1\nCHANGED\nline3\nline4\n").unwrap();
    let unstaged = ok(alt(root, &["diff"]));
    assert!(
        unstaged.contains("diff --git a/f.txt b/f.txt"),
        "{unstaged}"
    );
    assert!(unstaged.contains("--- a/f.txt"), "{unstaged}");

    // cross-check the hunk body against real git on the same edit
    let gdir = tempfile::tempdir().unwrap();
    let groot = gdir.path();
    ok(git(groot, &["init", "-q", "."]));
    ok(git(groot, &["config", "user.email", "t@e"]));
    ok(git(groot, &["config", "user.name", "t"]));
    std::fs::write(groot.join("f.txt"), "line1\nline2\nline3\n").unwrap();
    ok(git(groot, &["add", "."]));
    ok(git(groot, &["commit", "-qm", "first"]));
    std::fs::write(groot.join("f.txt"), "line1\nCHANGED\nline3\nline4\n").unwrap();
    let gdiff = ok(git(groot, &["-c", "core.pager=cat", "diff"]));
    assert_eq!(
        hunk_body(&unstaged),
        hunk_body(&gdiff),
        "hunk body must match git"
    );

    // before staging, --cached is empty; after add it shows the staged change
    assert!(
        ok(alt(root, &["diff", "--cached"])).is_empty(),
        "nothing staged yet"
    );
    ok(alt(root, &["add", "."]));
    let cached = ok(alt(root, &["diff", "--cached"]));
    assert_eq!(
        hunk_body(&cached),
        hunk_body(&gdiff),
        "staged hunk matches git"
    );
    // and now the working tree matches the index, so unstaged is empty
    assert!(ok(alt(root, &["diff"])).is_empty(), "work tree == index");

    // a new staged file is a full addition
    std::fs::write(root.join("new.txt"), "alpha\nbeta\n").unwrap();
    ok(alt(root, &["add", "."]));
    let added = ok(alt(root, &["diff", "--cached"]));
    assert!(added.contains("new file mode 100644"), "{added}");
    assert!(added.contains("--- /dev/null"), "{added}");
    assert!(added.contains("+alpha\n+beta\n"), "{added}");

    // binary content is reported, not dumped
    std::fs::write(root.join("b.bin"), b"\x00\x01\x02bin\x00").unwrap();
    ok(alt(root, &["add", "."]));
    let bin = ok(alt(root, &["diff", "--cached"]));
    assert!(
        bin.contains("Binary files a/b.bin and b/b.bin differ"),
        "{bin}"
    );
}
