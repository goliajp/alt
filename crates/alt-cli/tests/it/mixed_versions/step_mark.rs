//! A step the new program was killed in, and the older program in the
//! workspace before the new one comes back: while the step is open it is
//! refused every command that writes, and can at most finish a checkout
//! the step had begun; once the new program has closed the step it works
//! on, and the merge the two of them brought to an end is a merge. Where a
//! power cut took the guard of the step and left its mark, the older
//! program writes past the mark, and what it leaves is not written over.

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

fn refused_to_write(old: &Path, repo: &Path) {
    for args in [
        &["branch", "kept"][..],
        &["add", "f.txt"],
        &["undo"],
        &["commit", "-m", "m"],
    ] {
        let refused = run(old, repo, args);
        assert_eq!(
            refused.status.code(),
            Some(75),
            "{args:?}: {}",
            said(&refused)
        );
        assert!(
            said(&refused).contains("recovery required"),
            "{}",
            said(&refused)
        );
    }
}

/// Kills a merge at the point `name` the `nth` time it passes it.
fn merge_killed_at(repo: &Path, name: &str) {
    let points = points_of_a_merge();
    let at = points.iter().position(|p| p == name).unwrap();
    killed(repo, &["merge", "feature/side"], at + 1);
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn the_older_program_is_refused_while_a_step_is_open_and_works_on_once_it_is_closed() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    diverged(repo);
    let status = || ok(run(&new, repo, &["status", "--json"]));
    let clean = status();
    merge_killed_at(repo, "step-mark:after");

    // the guard of the step is all the older program sees of it: a
    // checkout it must not write past and cannot finish
    refused_to_write(&old, repo);
    let pending = ok(run(&old, repo, &["operation", "pending"]));
    assert_eq!(pending.lines().count(), 1, "{pending}");
    let guard = pending.split(' ').next().unwrap();
    let refused = run(&old, repo, &["operation", "recover", guard]);
    assert!(
        said(&refused).contains("index matches neither"),
        "{}",
        said(&refused)
    );
    refused_to_write(&old, repo);
    same_reads(&old, repo);

    // the new program closes the step, which had changed nothing
    let next = run(&new, repo, &["branch", "kept"]);
    assert_eq!(said(&next), "");
    ok(next);
    assert_eq!(status(), clean);
    assert_eq!(ok(run(&new, repo, &["operation", "pending"])), "");
    assert_eq!(ok(run(&old, repo, &["operation", "pending"])), "");
    ok(run(&old, repo, &["branch", "-d", "kept"]));
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
    // the intent of the conflict is in the log and synced; the file that
    // says a merge is under way is not written, nor any file of the conflict
    merge_killed_at(repo, "checkout-intent:after");
    assert!(!repo.join(".alt/MERGE_HEAD").exists());

    // the older program finishes the checkout, and is still kept from
    // writing: it cannot commit a conflict it takes for no merge
    refused_to_write(&old, repo);
    let pending = ok(run(&old, repo, &["operation", "pending"]));
    let ids: Vec<&str> = pending
        .lines()
        .map(|l| l.split(' ').next().unwrap())
        .collect();
    assert_eq!(ids.len(), 2, "{pending}");
    let recovered = ids
        .iter()
        .map(|id| run(&old, repo, &["operation", "recover", id]));
    let recovered: Vec<bool> = recovered.map(|o| o.status.success()).collect();
    assert_eq!(recovered, [false, true], "the guard, then the conflict");
    let by_old = ok(run(&old, repo, &["status", "--json"]));
    assert!(by_old.contains(r#""unmerged":["f.txt"]"#), "{by_old}");
    assert!(!repo.join(".alt/MERGE_HEAD").exists());
    refused_to_write(&old, repo);

    // the new program puts the state file as the log says and closes the
    // step: a merge to both from here on
    let next = run(&new, repo, &["branch", "kept"]);
    assert_eq!(said(&next), "");
    ok(next);
    assert!(repo.join(".alt/MERGE_HEAD").exists());
    let merging = status();
    assert!(merging.contains(r#""unmerged":["f.txt"]"#), "{merging}");
    assert!(
        merging.contains(r#""operation":{"kind":"merge"}"#),
        "{merging}"
    );
    same_reads(&old, repo);
    std::fs::write(repo.join("f.txt"), "both\n").unwrap();
    ok(run(&old, repo, &["add", "f.txt"]));
    ok(run(&old, repo, &["commit", "-m", "merged"]));
    let merged = ok(run(&new, repo, &["cat-file", "-p", "HEAD"]));
    assert_eq!(merged.matches("\nparent ").count(), 2, "{merged}");
    same_reads(&old, repo);
}

/// A step whose mark is in the log and whose guard a power cut took.
fn marked_without_its_guard(repo: &Path) {
    diverged(repo);
    merge_killed_at(repo, "step-mark:after");
    crate::crash_fixture::remove_final_log_frame(repo);
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn the_older_program_writes_after_a_mark_without_its_guard_and_the_step_is_left_as_it_is() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    marked_without_its_guard(repo);
    let status = || ok(run(&new, repo, &["status", "--json"]));
    let clean = status();
    ok(run(&old, repo, &["branch", "kept"]));
    same_reads(&old, repo);

    // the new program closes the mark and touches nothing
    let next = run(&new, repo, &["branch", "-d", "kept"]);
    let warned = "was interrupted and another program has written since: it is left as it is;";
    assert!(said(&next).contains(warned), "{}", said(&next));
    ok(next);
    assert_eq!(status(), clean);
    assert_eq!(ok(run(&new, repo, &["operation", "pending"])), "");
    // and there is nothing of the step to undo
    let undone = run(&new, repo, &["undo"]);
    assert!(!said(&undone).contains("Undid"), "{}", said(&undone));
    same_reads(&old, repo);
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn an_index_change_of_the_older_program_after_a_mark_is_no_reason_to_leave_the_step() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    marked_without_its_guard(repo);
    std::fs::write(repo.join("g.txt"), "g\n").unwrap();
    ok(run(&old, repo, &["add", "g.txt"]));

    let next = run(&new, repo, &["branch", "kept"]);
    assert_eq!(said(&next), "");
    ok(next);
    assert_eq!(ok(run(&new, repo, &["operation", "pending"])), "");
    let status = ok(run(&new, repo, &["status", "--json"]));
    assert!(status.contains(r#""operation":null"#), "{status}");
    assert!(status.contains("g.txt"), "{status}");
    same_reads(&old, repo);
}

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_state_file_that_is_not_as_the_log_says_keeps_the_step_open_until_it_is_closed_by_hand() {
    let (old, new) = (old_bin(), new_bin());
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    marked_without_its_guard(repo);
    ok(run(&old, repo, &["branch", "kept"]));
    // the log tells of no merge under way, and the file says there is one
    let side = ok(run(&new, repo, &["rev-parse", "feature/side"]));
    std::fs::write(repo.join(".alt/MERGE_HEAD"), &side).unwrap();

    let not_as_said = "MERGE_HEAD is not as the operation log says";
    for args in [&["commit", "-m", "m"][..], &["add", "f.txt"]] {
        let refused = run(&new, repo, args);
        assert_eq!(
            refused.status.code(),
            Some(75),
            "{args:?}: {}",
            said(&refused)
        );
        assert!(
            said(&refused).contains(not_as_said),
            "{args:?}: {}",
            said(&refused)
        );
    }
    // the abort would give the step up, which cannot be done either
    let refused = run(&new, repo, &["merge", "--abort"]);
    assert!(!refused.status.success());
    let how = "close it with `alt operation recover";
    assert!(said(&refused).contains(how), "{}", said(&refused));
    let pending = ok(run(&new, repo, &["operation", "pending"]));
    let id = pending.split(' ').next().unwrap();
    let refused = run(&new, repo, &["operation", "abandon", id]);
    assert!(
        said(&refused).contains("cannot be given up"),
        "{}",
        said(&refused)
    );
    assert!(repo.join(".alt/MERGE_HEAD").exists());

    // closed by the command that is asked to, with what is not as said
    let closed = run(&new, repo, &["operation", "recover", id]);
    assert!(said(&closed).contains(not_as_said), "{}", said(&closed));
    ok(closed);
    assert_eq!(ok(run(&new, repo, &["operation", "pending"])), "");
    ok(run(&new, repo, &["merge", "--abort"]));
    assert!(!repo.join(".alt/MERGE_HEAD").exists());
    same_reads(&old, repo);
}
