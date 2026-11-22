//! A revert of two commits in one workspace, and a commit of another
//! workspace between the two it makes: the notes ref the workspaces share
//! holds a note of the other between the two of the revert. Taking the
//! revert back, by an undo or by giving it up, takes its two notes out
//! and leaves the note of the other workspace. Whoever takes it back, the
//! notes commit that does is by the committer of the revert: a caller
//! nothing names takes it back as well.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn command(at: &Path, args: &[&str], crash: Option<&str>) -> Command {
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
    command
}

/// [`command`] by a caller neither the environment nor any configuration
/// names.
fn unnamed(at: &Path, args: &[&str], crash: Option<&str>) -> Command {
    let mut command = command(at, args, crash);
    for (name, _) in crate::TEST_SIGNING {
        command.env_remove(name);
    }
    command.env_remove("EMAIL");
    command
}

/// Who made the two commits of the revert, and so wrote their notes.
const REVERTER: [(&str, &str); 2] = [
    ("GIT_COMMITTER_NAME", "Reverter"),
    ("GIT_COMMITTER_EMAIL", "reverter@test.invalid"),
];

/// The notes commit at the tip is by the committer of the revert.
fn the_notes_are_taken_back_by_the_reverter(main: &Path) {
    let tip = ok(main, &["cat-file", "-p", "refs/notes/alt/meta"]);
    let by = |line: &&str| line.starts_with("author ") || line.starts_with("committer ");
    let by: Vec<&str> = tip.lines().filter(by).collect();
    assert_eq!(by.len(), 2, "{tip}");
    for line in by {
        let who = line.split_once(' ').unwrap().1;
        assert!(
            who.starts_with("Reverter <reverter@test.invalid> "),
            "{tip}"
        );
    }
}

fn alt(at: &Path, args: &[&str], crash: Option<&str>) -> Output {
    command(at, args, crash).output().unwrap()
}

fn ok(at: &Path, args: &[&str]) -> String {
    let done = alt(at, args, None);
    let said = String::from_utf8_lossy(&done.stderr);
    assert!(done.status.success(), "{args:?}: {said}");
    String::from_utf8(done.stdout).unwrap()
}

fn commit(at: &Path, file: &str) {
    std::fs::write(at.join(file), format!("{file}\n")).unwrap();
    ok(at, &["add", file]);
    ok(at, &["commit", "-m", file]);
}

const REVERT: &[&str] = &["revert", "HEAD", "HEAD~1"];

/// Two workspaces, and in `main` the revert of its last two commits with
/// a commit of `other` between the two commits the revert makes where
/// `between` is set. The
/// revert dies at the first point named `dies_at` after the second of its
/// ref transactions, where one is named. The workspaces, and the commits
/// the revert made.
fn reverted_around_a_commit(dir: &Path, dies_at: Option<&str>) -> (PathBuf, PathBuf, [String; 2]) {
    reverted(dir, dies_at, true)
}

fn reverted(dir: &Path, dies_at: Option<&str>, between: bool) -> (PathBuf, PathBuf, [String; 2]) {
    let (main, other) = (dir.join("main"), dir.join("other"));
    std::fs::create_dir(&main).unwrap();
    ok(&main, &["init", "."]);
    for file in ["f.txt", "g.txt", "h.txt"] {
        commit(&main, file);
    }
    ok(&main, &["branch", "feat"]);
    ok(
        &main,
        &["workspace", "add", "other", other.to_str().unwrap(), "feat"],
    );

    let list = dir.join("points");
    let listing = format!("list:{}", list.display());
    assert!(alt(&main, REVERT, Some(&listing)).status.success());
    ok(&main, &["undo"]);
    let points = std::fs::read_to_string(&list).unwrap();
    let points: Vec<&str> = points.lines().collect();
    let named = |name: &'static str| move |(_, point): &(usize, &&str)| **point == name;
    let mut transactions = points
        .iter()
        .enumerate()
        .filter(named("ref-transaction:before"));
    let second = transactions.nth(1).expect("two commits").0;
    let dies = dies_at.map_or(0, |name| {
        let later = points.iter().enumerate().skip(second + 1);
        later
            .filter(|(_, point)| **point == name)
            .map(|(at, _)| at + 1)
            .next()
            .unwrap()
    });

    let waits = dir.join("waits");
    let asked = format!("wait:{}:{dies}:{}", second + 1, waits.display());
    let mut revert = command(&main, REVERT, Some(&asked));
    let mut revert = revert
        .envs(REVERTER)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    while !waits.exists() {
        assert_eq!(revert.try_wait().unwrap(), None, "the revert did not wait");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    if between {
        commit(&other, "o.txt");
    }
    std::fs::remove_file(&waits).unwrap();
    let ended = revert.wait().unwrap();
    assert_eq!(ended.success(), dies_at.is_none(), "{ended}");
    let made = ["HEAD~1", "HEAD"].map(|rev| ok(&main, &["rev-parse", rev]).trim().to_owned());
    (main, other, made)
}

/// The note of the other workspace is there, with those of the commits
/// from before the revert, and the two of the revert are not.
fn only_the_notes_of_the_revert_are_gone(main: &Path, other: &Path, made: &[String; 2]) {
    let theirs = ok(other, &["log", "-n", "1"]);
    let theirs = theirs.split(' ').next().unwrap();
    assert!(ok(main, &["meta", "show", theirs]).contains("o.txt"));
    assert!(ok(main, &["meta", "show"]).contains("h.txt"));
    assert!(ok(main, &["meta", "show", "HEAD~1"]).contains("g.txt"));
    for commit in made {
        let shown = alt(main, &["meta", "show", commit], None);
        let said = String::from_utf8_lossy(&shown.stderr);
        assert!(said.contains("no metadata recorded"), "{commit}: {said}");
    }
    assert!(main.join("g.txt").exists() && main.join("h.txt").exists());
    assert!(ok(main, &["status", "--json"]).contains(r#""clean":true"#));
    ok(main, &["fsck"]);
    commit(main, "i.txt");
    assert!(ok(main, &["meta", "show"]).contains("i.txt"));
    assert!(ok(main, &["meta", "show", theirs]).contains("o.txt"));
}

#[test]
fn an_undo_leaves_the_note_another_workspace_wrote_between_the_commits_of_a_revert() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (main, other, made) = reverted_around_a_commit(dir.path(), None);
    for commit in &made {
        assert!(ok(&main, &["meta", "show", commit]).contains("Revert"));
    }
    ok(&main, &["undo"]);
    only_the_notes_of_the_revert_are_gone(&main, &other, &made);
}

#[test]
fn giving_a_revert_up_leaves_the_note_another_workspace_wrote_between_its_commits() {
    let dir = alt_os::temp::tempdir().unwrap();
    let dies_at = Some("checkout-file:before");
    let (main, other, made) = reverted_around_a_commit(dir.path(), dies_at);
    assert!(main.join("g.txt").exists());
    let pending = ok(&main, &["operation", "pending"]);
    let id = pending.split(' ').next().unwrap();
    ok(&main, &["operation", "abandon", id]);
    assert_eq!(ok(&main, &["operation", "pending"]), "");
    only_the_notes_of_the_revert_are_gone(&main, &other, &made);
}

fn committed_while_it_waits(
    dir: &Path,
    both: [&Path; 2],
    args: &[&str],
    points: &str,
    file: &str,
) -> String {
    committed_while_it_waits_by(command, dir, both, (args, points), file)
}

/// Runs `args`, by the caller `by` makes commands of, in `main` and
/// commits `file` in `other` while the command waits at the first ref
/// transaction among the `points` it passes. The commit `other` made.
fn committed_while_it_waits_by(
    by: fn(&Path, &[&str], Option<&str>) -> Command,
    dir: &Path,
    [main, other]: [&Path; 2],
    (args, points): (&[&str], &str),
    file: &str,
) -> String {
    let at = points.lines().position(|p| p == "ref-transaction:before");
    let waits = dir.join(format!("waits-{file}"));
    let asked = format!("wait:{}:0:{}", at.unwrap() + 1, waits.display());
    let mut waiting = by(main, args, Some(&asked));
    let mut waiting = waiting.stderr(Stdio::piped()).spawn().unwrap();
    while !waits.exists() {
        assert_eq!(waiting.try_wait().unwrap(), None, "{args:?} did not wait");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    commit(other, file);
    std::fs::remove_file(&waits).unwrap();
    let ended = waiting.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&ended.stderr);
    assert!(ended.status.success(), "{args:?}: {said}");
    let theirs = ok(other, &["log", "-n", "1"]);
    theirs.split(' ').next().unwrap().to_owned()
}

/// The notes of the two commits of the revert are gone, and every other
/// is there: of the commits before it, and of both commits of `other`.
fn the_notes_of_the_others_stay(main: &Path, made: &[String; 2], theirs: &[(&str, &str)]) {
    for (commit, file) in theirs {
        assert!(ok(main, &["meta", "show", commit]).contains(file), "{file}");
    }
    assert!(ok(main, &["meta", "show"]).contains("h.txt"));
    for commit in made {
        let shown = alt(main, &["meta", "show", commit], None);
        let said = String::from_utf8_lossy(&shown.stderr);
        assert!(said.contains("no metadata recorded"), "{commit}: {said}");
    }
    assert_eq!(ok(main, &["operation", "pending"]), "");
    assert!(ok(main, &["status", "--json"]).contains(r#""clean":true"#));
    ok(main, &["fsck"]);
}

#[test]
fn an_undo_taken_back_has_its_notes_again_and_one_goes_through_while_another_workspace_commits() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (main, other, made) = reverted_around_a_commit(dir.path(), None);
    let first = ok(&other, &["log", "-n", "1"]);
    let first = first.split(' ').next().unwrap();
    // each workspace is shown the note of its own head
    assert!(ok(&other, &["meta", "show"]).contains("o.txt"));
    assert!(ok(&main, &["meta", "show"]).contains("Revert"));

    let list = dir.path().join("undo-points");
    let listing = format!("list:{}", list.display());
    assert!(alt(&main, &["undo"], Some(&listing)).status.success());
    assert!(ok(&main, &["meta", "show"]).contains("h.txt"));
    // the undo taken back: the commits of the revert have their notes again
    ok(&main, &["undo"]);
    assert_eq!(ok(&main, &["rev-parse", "HEAD"]).trim(), made[1]);
    for commit in &made {
        assert!(ok(&main, &["meta", "show", commit]).contains("Revert"));
    }
    assert!(ok(&main, &["meta", "show", first]).contains("o.txt"));

    // and taken back once more while the other workspace writes a note
    let points = std::fs::read_to_string(&list).unwrap();
    let both = [main.as_path(), other.as_path()];
    let second = committed_while_it_waits(dir.path(), both, &["undo"], &points, "p.txt");
    the_notes_of_the_others_stay(&main, &made, &[(first, "o.txt"), (&second, "p.txt")]);
    // nothing but the undo is in the log of it: the next takes it back
    ok(&main, &["undo"]);
    assert_eq!(ok(&main, &["rev-parse", "HEAD"]).trim(), made[1]);
    assert!(ok(&main, &["meta", "show", &second]).contains("p.txt"));
}

#[test]
fn a_revert_is_given_up_while_another_workspace_commits() {
    let dir = alt_os::temp::tempdir().unwrap();
    let dies_at = Some("checkout-file:before");
    let (main, other, made) = reverted_around_a_commit(dir.path(), dies_at);
    let first = ok(&other, &["log", "-n", "1"]);
    let first = first.split(' ').next().unwrap();
    let pending = ok(&main, &["operation", "pending"]);
    let id = pending.split(' ').next().unwrap();
    let abandon = ["operation", "abandon", id];

    let (list, kept) = (dir.path().join("abandon-points"), dir.path().join("kept"));
    crate::step_crash::bench::copy(&main, &kept);
    let listing = format!("list:{}", list.display());
    assert!(alt(&main, &abandon, Some(&listing)).status.success());
    crate::step_crash::bench::restore(&main, &kept);
    let points = std::fs::read_to_string(&list).unwrap();
    let both = [main.as_path(), other.as_path()];
    let second = committed_while_it_waits(dir.path(), both, &abandon, &points, "p.txt");
    the_notes_of_the_others_stay(&main, &made, &[(first, "o.txt"), (&second, "p.txt")]);
    assert!(main.join("g.txt").exists() && main.join("h.txt").exists());
    // the mark that gave it up is the one the transaction came after
    let marks = ok(&main, &["op-log"]).matches("recover").count();
    crate::step_crash::bench::restore(&main, &kept);
    ok(&main, &abandon);
    assert_eq!(ok(&main, &["op-log"]).matches("recover").count(), marks);
}

#[test]
fn the_daemon_takes_the_notes_of_a_revert_out_on_top_of_those_of_another_workspace() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (main, other, made) = reverted_around_a_commit(dir.path(), None);
    let theirs = ok(&other, &["log", "-n", "1"]);
    let theirs = theirs.split(' ').next().unwrap();
    let undone = crate::scan_daemon::output(&main, &["undo"], true);
    let _stop = crate::scan_daemon::Stop(crate::scan_daemon::daemon_pid(&main), false);
    let said = String::from_utf8_lossy(&undone.stderr);
    assert!(undone.status.success(), "{said}");
    assert!(ok(&main, &["meta", "show", theirs]).contains("o.txt"));
    assert!(ok(&main, &["meta", "show"]).contains("h.txt"));
    for commit in &made {
        let shown = alt(&main, &["meta", "show", commit], None);
        let said = String::from_utf8_lossy(&shown.stderr);
        assert!(said.contains("no metadata recorded"), "{commit}: {said}");
    }
}

fn by_nobody(main: &Path, args: &[&str]) {
    let done = unnamed(main, args, None).output().unwrap();
    let said = String::from_utf8_lossy(&done.stderr);
    assert!(done.status.success(), "{args:?}: {said}");
}

#[test]
fn a_caller_nothing_names_undoes_a_revert_around_the_note_of_another_workspace() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (main, other, made) = reverted_around_a_commit(dir.path(), None);
    by_nobody(&main, &["undo"]);
    the_notes_are_taken_back_by_the_reverter(&main);
    only_the_notes_of_the_revert_are_gone(&main, &other, &made);
}

#[test]
fn a_caller_nothing_names_gives_a_revert_up_around_the_note_of_another_workspace() {
    let dir = alt_os::temp::tempdir().unwrap();
    let dies_at = Some("checkout-file:before");
    let (main, other, made) = reverted_around_a_commit(dir.path(), dies_at);
    let pending = ok(&main, &["operation", "pending"]);
    let id = pending.split(' ').next().unwrap();
    by_nobody(&main, &["operation", "abandon", id]);
    assert_eq!(ok(&main, &["operation", "pending"]), "");
    the_notes_are_taken_back_by_the_reverter(&main);
    only_the_notes_of_the_revert_are_gone(&main, &other, &made);
}

/// The notes ref is first found as the revert left it, to be moved back
/// as it is, and is moved by the other workspace before that is done: the
/// notes commit is written on the second try.
#[test]
fn a_caller_nothing_names_undoes_a_revert_while_another_workspace_commits() {
    let dir = alt_os::temp::tempdir().unwrap();
    let (main, other, made) = reverted(dir.path(), None, false);
    let list = dir.path().join("undo-points");
    let listing = format!("list:{}", list.display());
    assert!(alt(&main, &["undo"], Some(&listing)).status.success());
    ok(&main, &["undo"]);
    assert_eq!(ok(&main, &["rev-parse", "HEAD"]).trim(), made[1]);

    let points = std::fs::read_to_string(&list).unwrap();
    let both = [main.as_path(), other.as_path()];
    let asked: (&[&str], &str) = (&["undo"], &points);
    let theirs = committed_while_it_waits_by(unnamed, dir.path(), both, asked, "p.txt");
    the_notes_are_taken_back_by_the_reverter(&main);
    the_notes_of_the_others_stay(&main, &made, &[(&theirs, "p.txt")]);
}

#[test]
fn a_caller_nothing_names_gives_a_revert_up_while_another_workspace_commits() {
    let dir = alt_os::temp::tempdir().unwrap();
    let dies_at = Some("checkout-file:before");
    let (main, other, made) = reverted(dir.path(), dies_at, false);
    let pending = ok(&main, &["operation", "pending"]);
    let id = pending.split(' ').next().unwrap();
    let abandon = ["operation", "abandon", id];

    let (list, kept) = (dir.path().join("abandon-points"), dir.path().join("kept"));
    crate::step_crash::bench::copy(&main, &kept);
    let listing = format!("list:{}", list.display());
    assert!(alt(&main, &abandon, Some(&listing)).status.success());
    crate::step_crash::bench::restore(&main, &kept);
    let points = std::fs::read_to_string(&list).unwrap();
    let both = [main.as_path(), other.as_path()];
    let asked: (&[&str], &str) = (&abandon, &points);
    let theirs = committed_while_it_waits_by(unnamed, dir.path(), both, asked, "p.txt");
    the_notes_are_taken_back_by_the_reverter(&main);
    the_notes_of_the_others_stay(&main, &made, &[(&theirs, "p.txt")]);
    assert!(main.join("g.txt").exists() && main.join("h.txt").exists());
}
