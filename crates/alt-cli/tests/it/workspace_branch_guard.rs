//! A branch is checked out in at most one workspace: nothing may move,
//! delete or re-check-out a branch that another workspace holds, because
//! that workspace's index and working tree would silently fall behind its
//! HEAD and its next commit would undo the move.

use std::path::Path;
use std::process::{Command, Output};

fn alt(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(cwd)
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

/// Asserts the command failed and returns its stderr.
fn refused(o: Output) -> String {
    assert!(
        !o.status.success(),
        "expected a refusal, got stdout: {}",
        String::from_utf8_lossy(&o.stdout)
    );
    String::from_utf8(o.stderr).unwrap()
}

fn seed(root: &Path) {
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "seed"]));
}

fn tip(root: &Path, branch: &str) -> String {
    ok(alt(root, &["rev-parse", branch])).trim().to_owned()
}

#[test]
fn finishing_into_a_branch_checked_out_elsewhere_is_refused() {
    let repo = tempfile::tempdir().unwrap();
    let trees = tempfile::tempdir().unwrap();
    let root = repo.path();
    seed(root);
    ok(alt(root, &["flow", "init"]));
    ok(alt(root, &["switch", "develop"]));
    let develop_before = tip(root, "develop");

    let ws = trees.path().join("ws");
    ok(alt(root, &["workspace", "add", "ws", ws.to_str().unwrap()]));
    ok(alt(&ws, &["flow", "feature", "start", "x"]));
    std::fs::write(ws.join("b.txt"), "b\n").unwrap();
    ok(alt(&ws, &["add", "."]));
    ok(alt(&ws, &["commit", "-m", "work"]));

    let err = refused(alt(&ws, &["flow", "feature", "finish", "x"]));
    assert!(
        err.contains("branch 'develop' is checked out in workspace 'default'"),
        "{err}"
    );
    // nothing moved, so the default workspace is still in step with develop
    assert_eq!(tip(root, "develop"), develop_before);
    assert!(ok(alt(root, &["status"])).contains("nothing to commit"));

    // once the default workspace lets go of develop, the finish goes through
    ok(alt(root, &["switch", "main"]));
    ok(alt(&ws, &["flow", "feature", "finish", "x"]));
    assert_ne!(tip(root, "develop"), develop_before);
}

#[test]
fn a_branch_cannot_be_checked_out_twice_or_deleted_from_elsewhere() {
    let repo = tempfile::tempdir().unwrap();
    let trees = tempfile::tempdir().unwrap();
    let root = repo.path();
    seed(root);
    ok(alt(root, &["branch", "feat"]));
    let ws2 = trees.path().join("ws2");
    ok(alt(
        root,
        &["workspace", "add", "ws2", ws2.to_str().unwrap(), "feat"],
    ));

    // a second workspace on feat, refused before anything lands on disk
    let ws3 = trees.path().join("ws3");
    let err = refused(alt(
        root,
        &["workspace", "add", "ws3", ws3.to_str().unwrap(), "feat"],
    ));
    assert!(err.contains("checked out in workspace 'ws2'"), "{err}");
    assert!(
        !ws3.join(".alt").exists(),
        "no workspace marker may be left"
    );
    assert!(!ok(alt(root, &["workspace", "list"])).contains("ws3"));

    // the default workspace switching onto it
    let err = refused(alt(root, &["switch", "feat"]));
    assert!(err.contains("checked out in workspace 'ws2'"), "{err}");

    // ws2 deleting the branch the default workspace is on
    let err = refused(alt(&ws2, &["branch", "-d", "main"]));
    assert!(err.contains("checked out in workspace 'default'"), "{err}");
    assert!(ok(alt(root, &["branch"])).contains("main"));

    // ws2's own branch still moves freely
    std::fs::write(ws2.join("c.txt"), "c\n").unwrap();
    ok(alt(&ws2, &["add", "."]));
    ok(alt(&ws2, &["commit", "-m", "on feat"]));
}

#[test]
fn workspace_add_without_a_branch_makes_its_own() {
    let repo = tempfile::tempdir().unwrap();
    let trees = tempfile::tempdir().unwrap();
    let root = repo.path();
    seed(root);
    let main = tip(root, "main");

    let ws = trees.path().join("side");
    let out = ok(alt(
        root,
        &["workspace", "add", "side", ws.to_str().unwrap()],
    ));
    assert!(out.contains("on side"), "{out}");
    assert_eq!(tip(root, "side"), main, "the new branch starts at HEAD");
    assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "a\n");

    // the name is taken now: a second workspace of that name has no branch to make
    ok(alt(root, &["workspace", "remove", "side"]));
    let again = trees.path().join("again");
    let err = refused(alt(
        root,
        &["workspace", "add", "side", again.to_str().unwrap()],
    ));
    assert!(
        err.contains("a branch named 'side' already exists"),
        "{err}"
    );
}
