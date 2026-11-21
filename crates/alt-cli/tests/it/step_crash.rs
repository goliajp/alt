//! A command that begins an operation, goes on with it, ends it or takes
//! one of those back, killed at every point it passes: the next command
//! that writes leaves the workspace as it was before the command, as the
//! command leaves it, or with the operation under way and gone on with to
//! the same end. The undo after that takes the whole command back or is
//! refused.
//!
//! The points are those a debug build dies at when `ALT_CRASH_POINT` asks:
//! before and after the mark a step begins with, each ref transaction, the
//! intent of each checkout, each file a checkout writes, its index and its
//! completion, each state file written or removed, and the record of the
//! step.

use std::path::Path;

pub(crate) mod bench;
use bench::{Bench, abort_killed_before_its_state_file, copy, killed_and_its_undo, restore, run};

/// `main` and `side` part after one commit: both change `f.txt`; `side`
/// alone changes `t.txt`, adds `new.txt` and `in/a/n.txt`, removes `d.txt`.
fn diverged() -> Bench {
    let bench = Bench::new();
    run(&bench.repo(), &["init", "."], 0);
    bench.write("f.txt", "1\n2\n3\n");
    for file in ["t.txt", "u.txt", "d.txt"] {
        bench.write(file, &format!("{file}\n"));
    }
    bench.commit("files");
    run(&bench.repo(), &["switch", "-c", "side"], 0);
    bench.write("f.txt", "1\nside\n3\n");
    bench.write("t.txt", "t.txt by side\n");
    bench.write("new.txt", "new\n");
    bench.write("in/a/n.txt", "n\n");
    std::fs::remove_file(bench.repo().join("d.txt")).unwrap();
    bench.commit("side");
    run(&bench.repo(), &["switch", "main"], 0);
    bench.write("f.txt", "1\nmain\n3\n");
    bench.commit("main");
    bench
}

/// Five commits: reverting the last but one, the second and the last
/// reverts and commits the first of them, stops at the second, and leaves
/// the third waiting.
fn to_revert() -> Bench {
    let bench = Bench::new();
    run(&bench.repo(), &["init", "."], 0);
    let files = [
        ("f.txt", "1\n2\n3\n", "f"),
        ("f.txt", "1\nb\n3\n", "to b"),
        ("f.txt", "1\nc\n3\n", "to c"),
        ("h.txt", "h\n", "h"),
        ("i.txt", "i\n", "i"),
    ];
    for (file, text, message) in files {
        bench.write(file, text);
        bench.commit(message);
    }
    bench
}

/// [`diverged`] on `side` with two more commits there: rebasing on `main`
/// stops at the first commit of `side`, again at the second, and replays
/// the third.
fn to_rebase() -> Bench {
    let bench = diverged();
    run(&bench.repo(), &["switch", "side"], 0);
    bench.write("f.txt", "1\nside again\n3\n");
    bench.commit("side again");
    bench.write("s.txt", "s\n");
    bench.commit("side adds s");
    bench
}

/// A command and the exit code it ends with; `resolve` writes `f.txt` and
/// adds it.
type Step = (&'static [&'static str], i32);

const RESOLVE: Step = (&["resolve"], 0);
const MERGE: Step = (&["merge", "side"], 1);
const PICK: Step = (&["cherry-pick", "side"], 1);
const REVERT: Step = (&["revert", "HEAD~1", "HEAD~3", "HEAD"], 1);
const REBASE: Step = (&["rebase", "main"], 1);
const COMMIT: Step = (&["commit", "-m", "by hand"], 0);

fn through(repo: &Path, steps: &[Step]) {
    for (at, (args, exit)) in steps.iter().enumerate() {
        if *args == RESOLVE.0 {
            std::fs::write(repo.join("f.txt"), format!("resolved at {at}\n")).unwrap();
            run(repo, &["add", "f.txt"], 0);
        } else {
            run(repo, args, *exit);
        }
    }
}

/// [`killed_and_its_undo`] for `command` after `steps`; `resume` is the
/// command itself and its exit code where none is given.
fn case(bench: &Bench, steps: &[Step], command: Step, resume: Option<Step>) -> Vec<String> {
    copy(&bench.repo(), &bench.kept("fixture"));
    let setup = |repo: &Path| through(repo, steps);
    let (args, exit) = command;
    let (resume, ends) = resume.unwrap_or(command);
    let points = killed_and_its_undo(bench, &setup, args, exit, Some((resume, ends)));
    if args.contains(&"--abort") {
        assert_eq!(
            count(&points, "ref-transaction:before") + count(&points, "checkout-intent:before"),
            1
        );
        abort_killed_before_its_state_file(bench, &setup, args, &points);
    }
    points
}

fn count(points: &[String], name: &str) -> usize {
    points.iter().filter(|point| *point == name).count()
}

#[test]
fn a_merge_killed_as_it_stops_is_whole_or_not_at_all() {
    let points = case(&diverged(), &[], MERGE, None);
    // the conflict is written as a checkout is, after the file that says
    // a merge is under way
    let file = points.iter().position(|p| p == "state-file:after").unwrap();
    let wrote = points
        .iter()
        .position(|p| p == "checkout-file:before")
        .unwrap();
    assert!(file < wrote, "{points:?}");
    assert_eq!(count(&points, "checkout-file:before"), 5, "{points:?}");
    assert_eq!(count(&points, "step-mark:before"), 1, "{points:?}");
    assert_eq!(count(&points, "step-record:before"), 1, "{points:?}");
}

#[test]
fn a_merge_killed_as_it_is_given_up_is_whole_or_not_at_all() {
    case(&diverged(), &[MERGE], (&["merge", "--abort"], 0), None);
}

#[test]
fn a_merge_killed_as_a_commit_concludes_it_is_whole_or_not_at_all() {
    let bench = diverged();
    let points = case(&bench, &[MERGE, RESOLVE], COMMIT, None);
    assert_eq!(count(&points, "ref-transaction:before"), 1, "{points:?}");
    restore(&bench.repo(), &bench.kept("fixture"));
    through(&bench.repo(), &[MERGE, RESOLVE, COMMIT]);
    assert_eq!(bench.shape().head.1, 2, "the commit that concludes a merge");
}

#[test]
fn a_cherry_pick_killed_as_it_stops_is_whole_or_not_at_all() {
    case(&diverged(), &[], PICK, None);
}

#[test]
fn a_cherry_pick_killed_as_it_is_given_up_is_whole_or_not_at_all() {
    case(&diverged(), &[PICK], (&["cherry-pick", "--abort"], 0), None);
}

#[test]
fn a_cherry_pick_killed_as_it_is_gone_on_with_is_whole_or_not_at_all() {
    let go_on: Step = (&["cherry-pick", "--continue"], 0);
    case(&diverged(), &[PICK, RESOLVE], go_on, None);
    case(&diverged(), &[PICK, RESOLVE], COMMIT, None);
}

#[test]
fn a_revert_killed_as_it_stops_is_whole_or_gone_on_with() {
    // the first is reverted and committed, the second stops, the third waits
    let go_on: Step = (&["revert", "--continue"], 1);
    let points = case(&to_revert(), &[], REVERT, Some(go_on));
    assert_eq!(count(&points, "ref-transaction:before"), 1, "{points:?}");
}

#[test]
fn a_revert_killed_as_it_is_given_up_is_whole_or_not_at_all() {
    // the branch moves back with the files, in one ref transaction
    let points = case(&to_revert(), &[REVERT], (&["revert", "--abort"], 0), None);
    assert_eq!(count(&points, "ref-transaction:before"), 1, "{points:?}");
}

#[test]
fn a_revert_killed_as_it_is_gone_on_with_is_whole_or_gone_on_with() {
    // commits the one that stopped and the one that waited
    let go_on: Step = (&["revert", "--continue"], 0);
    let points = case(&to_revert(), &[REVERT, RESOLVE], go_on, None);
    assert_eq!(count(&points, "ref-transaction:before"), 2, "{points:?}");
    case(&to_revert(), &[REVERT, RESOLVE], COMMIT, None);
}

#[test]
fn a_rebase_killed_as_it_stops_is_whole_or_gone_on_with() {
    case(
        &to_rebase(),
        &[],
        REBASE,
        Some((&["rebase", "--continue"], 1)),
    );
}

#[test]
fn a_rebase_killed_as_it_is_given_up_is_whole_or_not_at_all() {
    // HEAD goes back to the branch with the files, in one ref transaction
    let points = case(&to_rebase(), &[REBASE], (&["rebase", "--abort"], 0), None);
    assert_eq!(count(&points, "ref-transaction:before"), 1, "{points:?}");
}

#[test]
fn a_rebase_killed_as_a_step_is_gone_on_with_or_skipped_is_whole_or_gone_on_with() {
    // commits the step and stops at the next
    let stops_again: Step = (&["rebase", "--continue"], 1);
    case(&to_rebase(), &[REBASE, RESOLVE], stops_again, None);
    // drops the step, replays the last and moves the branch
    let steps = [REBASE, RESOLVE, stops_again];
    let ends: Step = (&["rebase", "--continue"], 0);
    case(&to_rebase(), &steps, (&["rebase", "--skip"], 0), Some(ends));
}

#[test]
fn a_rebase_killed_as_its_last_step_is_committed_is_whole_or_gone_on_with() {
    let stops_again: Step = (&["rebase", "--continue"], 1);
    let steps = [REBASE, RESOLVE, stops_again, RESOLVE];
    case(&to_rebase(), &steps, COMMIT, None);
    let steps = [REBASE, RESOLVE, stops_again, RESOLVE, COMMIT];
    case(&to_rebase(), &steps, (&["rebase", "--continue"], 0), None);
}
