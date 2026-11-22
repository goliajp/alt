//! A command that goes on with what stopped on a conflict writes commits,
//! and is refused before it changes anything where nothing names who
//! writes them; one that gives the stopped thing up writes none and is
//! not asked who runs it.

use super::*;

/// `main` and `topic` each change the one line of `f` after `base`, and
/// `topic` goes on to add `g`. The commits `base` and the first of
/// `topic`.
fn conflicting(root: &Path, daemon: bool) -> [String; 2] {
    init(root);
    let run = |args: &[&str]| ok(alt(root, daemon, &LATER, args));
    let put = |text: &str| {
        std::fs::write(root.join("f"), text).unwrap();
        run(&["add", "f"]);
        run(&["commit", "-m", text.trim()]);
        run(&["rev-parse", "HEAD"]).trim().to_owned()
    };
    let base = put("base\n");
    run(&["branch", "topic"]);
    run(&["switch", "topic"]);
    let theirs = put("topic\n");
    std::fs::write(root.join("g"), "g\n").unwrap();
    run(&["add", "g"]);
    run(&["commit", "-m", "more"]);
    run(&["switch", "main"]);
    put("main\n");
    [base, theirs]
}

/// Runs what stops on the conflict in `f`.
fn stop(root: &Path, daemon: bool, args: &[&str]) {
    let stopped = alt(root, daemon, &LATER, args);
    assert!(!stopped.status.success(), "{args:?} did not stop");
    let status = ok(alt(root, false, &[], &["status"]));
    assert!(status.contains("Unmerged paths"), "{args:?} left {status}");
}

fn resolve(root: &Path, daemon: bool) {
    std::fs::write(root.join("f"), "resolved\n").unwrap();
    ok(alt(root, daemon, &[], &["add", "f"]));
}

const LACKING: &str = "name and email are not set";

/// By nobody, `args` is refused and leaves everything as it was.
fn refused_unchanged(root: &Path, daemon: bool, args: &[&str]) {
    let before = state(root);
    refused(alt(root, daemon, &[], args), LACKING);
    assert_eq!(state(root), before, "{args:?}, daemon {daemon}");
}

fn subjects(root: &Path) -> Vec<String> {
    let log = ok(alt(root, false, &[], &["log"]));
    let subject = |line: &str| line.split_once(' ').unwrap().1.to_owned();
    log.lines().map(subject).collect()
}

#[test]
fn a_rebase_is_neither_skipped_on_nor_continued_by_nobody() {
    for daemon in [false, true] {
        for skip in [true, false] {
            let dir = alt_os::temp::tempdir().unwrap();
            let root = dir.path();
            conflicting(root, daemon);
            ok(alt(root, daemon, &[], &["switch", "topic"]));
            stop(root, daemon, &["rebase", "main"]);
            refused_unchanged(root, daemon, &["rebase", "--skip"]);
            refused_unchanged(root, daemon, &["rebase", "--continue"]);
            resolve(root, daemon);
            refused_unchanged(root, daemon, &["rebase", "--skip"]);
            refused_unchanged(root, daemon, &["rebase", "--continue"]);
            let on = if skip { "--skip" } else { "--continue" };
            ok(alt(root, daemon, &LATER, &["rebase", on]));
            let expected: &[&str] = match skip {
                true => &["more", "main", "base", "seed"],
                false => &["more", "topic", "main", "base", "seed"],
            };
            assert_eq!(subjects(root), expected, "daemon {daemon}");
            let by = who(&alt_lines(root, "HEAD"))[1].to_owned();
            assert_eq!(by, "committer Third <third@x>");
        }
    }
}

#[test]
fn a_revert_and_a_cherry_pick_are_not_continued_by_nobody() {
    for daemon in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path();
        let [base, _] = conflicting(root, daemon);
        stop(root, daemon, &["revert", &base]);
        refused_unchanged(root, daemon, &["revert", "--continue"]);
        resolve(root, daemon);
        refused_unchanged(root, daemon, &["revert", "--continue"]);
        ok(alt(root, daemon, &LATER, &["revert", "--continue"]));
        assert_eq!(subjects(root).len(), 4, "daemon {daemon}");

        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path();
        let [_, theirs] = conflicting(root, daemon);
        stop(root, daemon, &["cherry-pick", &theirs]);
        refused_unchanged(root, daemon, &["cherry-pick", "--continue"]);
        resolve(root, daemon);
        refused_unchanged(root, daemon, &["cherry-pick", "--continue"]);
        ok(alt(root, daemon, &LATER, &["cherry-pick", "--continue"]));
        assert_eq!(subjects(root)[0], "topic", "daemon {daemon}");
    }
}

#[test]
fn the_note_of_a_commit_is_not_rewritten_by_nobody() {
    for daemon in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path();
        conflicting(root, daemon);
        refused_unchanged(root, daemon, &["meta", "set", "-m", "reworded"]);
        ok(alt(
            root,
            daemon,
            &LATER,
            &["meta", "set", "-m", "reworded"],
        ));
        let by = alt_lines(root, "refs/notes/alt/meta");
        assert_eq!(who(&by)[1], "committer Third <third@x>");
    }
}

#[test]
fn what_stopped_is_given_up_by_nobody() {
    for daemon in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path();
        let [base, theirs] = conflicting(root, daemon);
        let head = ok(alt(root, daemon, &[], &["rev-parse", "HEAD"]));
        for (begin, abort) in [
            (&["merge", "topic"][..], "merge"),
            (&["revert", &base], "revert"),
            (&["cherry-pick", &theirs], "cherry-pick"),
        ] {
            stop(root, daemon, begin);
            ok(alt(root, daemon, &[], &[abort, "--abort"]));
            assert_eq!(ok(alt(root, daemon, &[], &["rev-parse", "HEAD"])), head);
            let left = std::fs::read_to_string(root.join("f")).unwrap();
            assert_eq!(left, "main\n", "{abort}, daemon {daemon}");
        }
        ok(alt(root, daemon, &[], &["switch", "topic"]));
        let head = ok(alt(root, daemon, &[], &["rev-parse", "HEAD"]));
        stop(root, daemon, &["rebase", "main"]);
        ok(alt(root, daemon, &[], &["rebase", "--abort"]));
        assert_eq!(ok(alt(root, daemon, &[], &["rev-parse", "HEAD"])), head);
    }
}
