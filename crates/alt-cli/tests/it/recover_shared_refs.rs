//! A checkout that a ref transaction carries, killed in one workspace, and
//! a commit in another workspace before it is finished: the notes ref the
//! two share has moved on, and the checkout is finished all the same.

use std::path::Path;
use std::process::Output;

fn alt(at: &Path, args: &[&str], crash: Option<&str>) -> Output {
    let mut command = crate::alt_command();
    command
        .current_dir(at)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args);
    if let Some(crash) = crash {
        command.env("ALT_CRASH_POINT", crash);
    }
    command.output().unwrap()
}

fn ok(at: &Path, args: &[&str]) -> String {
    let done = alt(at, args, None);
    let said = String::from_utf8_lossy(&done.stderr);
    assert!(done.status.success(), "{args:?}: {said}");
    String::from_utf8(done.stdout).unwrap()
}

fn commit(at: &Path, file: &str, message: &str) {
    std::fs::write(at.join(file), format!("{file}\n")).unwrap();
    ok(at, &["add", file]);
    ok(at, &["commit", "-m", message]);
}

#[test]
fn a_revert_killed_in_one_workspace_is_finished_after_a_commit_in_another() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (main, other) = (dir.path().join("main"), dir.path().join("other"));
    std::fs::create_dir(&main).unwrap();
    ok(&main, &["init", "."]);
    commit(&main, "f.txt", "f");
    commit(&main, "g.txt", "g");
    ok(&main, &["branch", "feat"]);
    ok(
        &main,
        &["workspace", "add", "other", other.to_str().unwrap(), "feat"],
    );

    // the commit of the revert and its note are in the log, and the file
    // the revert removes is still there
    let list = dir.path().join("points");
    let whole = alt(
        &main,
        &["revert", "HEAD"],
        Some(&format!("list:{}", list.display())),
    );
    assert!(whole.status.success());
    let points = std::fs::read_to_string(&list).unwrap();
    let write = points.lines().position(|p| p == "checkout-file:before");
    ok(&main, &["undo"]);
    let killed = alt(
        &main,
        &["revert", "HEAD"],
        Some(&(write.unwrap() + 1).to_string()),
    );
    assert_eq!(killed.status.code(), None);
    assert!(main.join("g.txt").exists());

    // the other workspace puts the note of its commit on top
    commit(&other, "h.txt", "h");

    let pending = ok(&main, &["operation", "pending"]);
    let id = pending.split(' ').next().unwrap();
    ok(&main, &["operation", "recover", id]);
    assert!(!main.join("g.txt").exists());
    assert_eq!(ok(&main, &["operation", "pending"]), "");
    assert!(ok(&main, &["status", "--json"]).contains(r#""clean":true"#));
    // both notes are there, each shown in the workspace of its commit
    assert!(ok(&main, &["meta", "show"]).contains("summary: Revert"));
    assert!(ok(&other, &["meta", "show"]).contains("summary: h\n"));
    commit(&main, "i.txt", "i");
    ok(&main, &["fsck"]);
}
