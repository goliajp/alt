//! The native commit loop: init → add → commit → status, and the exported
//! repository is git-fsck clean with the right tree.

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

#[test]
fn init_add_commit_status_then_export_is_git_clean() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));

    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/b.txt"), "nested\n").unwrap();

    assert!(
        ok(alt(root, &["status"])).contains("Untracked"),
        "new files are untracked"
    );
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));
    assert!(
        ok(alt(root, &["status"])).contains("working tree clean"),
        "after commit the tree is clean"
    );
    assert!(ok(alt(root, &["log", "--pretty=oneline"])).contains("first"));

    // export to a git repo and let git judge it
    let out = tempfile::tempdir().unwrap();
    let target = out.path().join("exported");
    ok(alt(root, &["export", target.to_str().unwrap()]));
    assert!(
        git(&target, &["fsck", "--strict"]).status.success(),
        "exported repo must be git-fsck clean"
    );
    let ls = ok(git(&target, &["ls-tree", "-r", "HEAD"]));
    assert!(
        ls.contains("\ta.txt") && ls.contains("\tsub/b.txt"),
        "tree: {ls}"
    );
}
