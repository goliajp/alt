//! A step the new program was killed in, and the older program at work
//! in the workspace before the new one comes back: it passes the mark of
//! the step over when it writes, refuses to undo it, and what it leaves
//! is not written over.

use std::path::Path;
use std::process::{Command, Output};

use super::abort::diverged;
use super::{new_bin, ok, old_bin, run, same_reads};

/// Runs the new program and kills it at the `at`-th of the points it passes.
fn killed(repo: &Path, args: &[&str], at: usize) {
    let killed = crashing(repo, args, &at.to_string());
    assert_eq!(killed.status.code(), None, "{args:?} at {at}");
}

fn crashing(repo: &Path, args: &[&str], crash: &str) -> Output {
    Command::new(new_bin())
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("ALT_CRASH_POINT", crash)
        .args(args)
        .output()
        .unwrap()
}

/// The points a merge that stops passes, by name.
fn points_of_a_merge() -> Vec<String> {
    let dir = alt_os::temp::tempdir().unwrap();
    diverged(dir.path());
    let list = dir.path().join(".alt/points");
    let listed = crashing(
        dir.path(),
        &["merge", "feature/side"],
        &format!("list:{}", list.display()),
    );
    assert_eq!(listed.status.code(), Some(1));
    let points = std::fs::read_to_string(list).unwrap();
    points.lines().map(str::to_owned).collect()
}

fn said(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn the_older_program_writes_after_a_mark_and_the_step_is_left_as_it_is() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    diverged(repo);
    let status = || ok(run(&new, repo, &["status", "--json"]));
    let clean = status();
    let points = points_of_a_merge();
    let marked = points.iter().position(|p| p == "step-mark:after").unwrap();
    killed(repo, &["merge", "feature/side"], marked + 1);

    // the mark is the last record: the older program refuses to undo it
    let refused = run(&old, repo, &["undo"]);
    assert!(!refused.status.success());
    assert!(
        said(&refused).contains("automatic undo refused"),
        "{}",
        said(&refused)
    );
    // and writes past it
    ok(run(&old, repo, &["branch", "kept"]));
    same_reads(&old, repo);

    // the new program closes the mark and touches nothing
    let next = run(&new, repo, &["branch", "-d", "kept"]);
    assert!(
        said(&next).contains("was interrupted and another program"),
        "{}",
        said(&next)
    );
    ok(next);
    assert_eq!(status(), clean);
    assert!(!repo.join(".alt/MERGE_HEAD").exists());
    assert_eq!(ok(run(&new, repo, &["operation", "pending"])), "");
    same_reads(&old, repo);
    // the merge is begun anew and taken back whole
    let stopped = run(&new, repo, &["merge", "feature/side"]);
    assert_eq!(stopped.status.code(), Some(1));
    ok(run(&new, repo, &["undo"]));
    assert_eq!(status(), clean);
    same_reads(&old, repo);
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_conflict_the_older_program_finished_writing_is_a_merge_to_both() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    diverged(repo);
    let status = || ok(run(&new, repo, &["status", "--json"]));
    let clean = status();
    let points = points_of_a_merge();
    // the intent of the conflict is in the log, and no file of it written
    let wrote = points
        .iter()
        .position(|p| p == "checkout-file:before")
        .unwrap();
    killed(repo, &["merge", "feature/side"], wrote + 1);
    // the file that says a merge is under way is there before the conflict
    assert!(repo.join(".alt/MERGE_HEAD").exists());

    // the older program finds a checkout to finish before it writes
    let refused = run(&old, repo, &["branch", "kept"]);
    assert!(
        said(&refused).contains("recovery required"),
        "{}",
        said(&refused)
    );
    let pending = ok(run(&old, repo, &["operation", "pending"]));
    let id = pending.split(' ').next().unwrap();
    ok(run(&old, repo, &["operation", "recover", id]));
    let by_old = ok(run(&old, repo, &["status", "--json"]));
    assert!(by_old.contains(r#""unmerged":["f.txt"]"#), "{by_old}");

    // the new program writes the record of the step: the merge is one
    // step, taken back whole
    let merging = status();
    assert!(merging.contains(r#""unmerged":["f.txt"]"#), "{merging}");
    assert!(
        merging.contains(r#""operation":{"kind":"merge"}"#),
        "{merging}"
    );
    ok(run(&new, repo, &["undo"]));
    assert_eq!(status(), clean);
    assert!(!repo.join(".alt/MERGE_HEAD").exists());
    same_reads(&old, repo);
}
