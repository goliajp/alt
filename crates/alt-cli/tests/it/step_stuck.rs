//! A merge, revert or rebase command that fails in the middle of a
//! checkout, or would have to write where a directory holds something:
//! what can be told beforehand refuses it before anything is written;
//! what cannot leaves the command to be brought to an end by the next
//! command that writes, as one that was killed is, or to be given up
//! whole where that cannot be done. Nothing takes back a part of it.
//!
//! A checkout is made to fail by a limit on the size of the files the
//! process may write, with a file larger than that among those it writes.

use std::path::Path;
use std::process::Output;

use crate::step_crash::bench::{Bench, alt, copy, restore, run, said};

const LARGE: usize = 1 << 20;

/// `alt` under a limit of 512 blocks on the files it writes.
pub(crate) fn limited(repo: &Path, args: &[&str], crash: Option<&str>) -> Output {
    let alt = crate::alt_command();
    let mut command = std::process::Command::new("sh");
    command
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("XDG_CONFIG_HOME", repo.join(".alt/test-config"))
        .args(["-c", r#"trap "" XFSZ; ulimit -f 512; exec "$0" "$@""#])
        .arg(alt.get_program())
        .args(args);
    if let Some(crash) = crash {
        command.env("ALT_CRASH_POINT", crash);
    }
    command.output().unwrap()
}

pub(crate) fn pending(repo: &Path) -> Vec<String> {
    let listed = run(repo, &["operation", "pending"], 0);
    let id = |line: &str| line.split(' ').next().unwrap().to_owned();
    listed.lines().map(id).collect()
}

/// `main` and `side` part after one commit: both change `f.txt`; `side`
/// adds `new.txt` and makes `z.txt` another large file.
pub(crate) fn diverged() -> Bench {
    let bench = Bench::new();
    run(&bench.repo(), &["init", "."], 0);
    bench.write("f.txt", "1\n2\n3\n");
    bench.write("z.txt", &"a".repeat(LARGE));
    bench.commit("files");
    run(&bench.repo(), &["switch", "-c", "side"], 0);
    bench.write("f.txt", "1\nside\n3\n");
    bench.write("new.txt", "new\n");
    bench.write("z.txt", &"b".repeat(LARGE));
    bench.commit("side");
    run(&bench.repo(), &["switch", "main"], 0);
    bench.write("f.txt", "1\nmain\n3\n");
    bench.commit("main");
    bench
}

/// Four commits: reverting the last and the second reverts and commits
/// the first of them, which makes `z.txt` the large file it was, and
/// stops at the second.
fn to_revert() -> Bench {
    let bench = Bench::new();
    run(&bench.repo(), &["init", "."], 0);
    let large = "b".repeat(LARGE);
    let files = [
        ("f.txt", "1\n2\n3\n"),
        ("f.txt", "1\nb\n3\n"),
        ("f.txt", "1\nc\n3\n"),
        ("z.txt", large.as_str()),
    ];
    bench.write("z.txt", &"a".repeat(LARGE));
    for (file, text) in files {
        bench.write(file, text);
        bench.commit(file);
    }
    bench
}

pub(crate) const MERGE: &[&str] = &["merge", "side"];
const REVERT: &[&str] = &["revert", "HEAD", "HEAD~2"];

/// The workspace `bench` holds, kept as `began`, with every file the
/// command writes stored for a checkout already: the command was run and
/// taken back. What the workspace is before the command and after it.
fn begun(bench: &Bench, command: &[&str], abort: &[&str]) -> [crate::step_crash::bench::Shape; 2] {
    let repo = bench.repo();
    let before = bench.shape();
    run(&repo, command, 1);
    let done = bench.shape();
    run(&repo, abort, 0);
    assert_eq!(bench.shape(), before);
    copy(&repo, &bench.kept("began"));
    [before, done]
}

/// The command fails at the large file: it is left as one that was
/// killed there, with its checkout to be finished.
pub(crate) fn stuck(bench: &Bench, command: &[&str]) {
    let repo = bench.repo();
    restore(&repo, &bench.kept("began"));
    let failed = limited(&repo, command, None);
    assert!(
        failed.status.code().is_some_and(|code| code != 0),
        "{}",
        said(&failed)
    );
    assert!(
        said(&failed).contains("cannot write 'z.txt'"),
        "{}",
        said(&failed)
    );
    assert!(
        !pending(&repo).is_empty(),
        "{command:?} left nothing to recover"
    );
}

/// What a command that writes says while the checkout cannot be finished.
pub(crate) fn refused(tried: &Output) {
    assert_eq!(tried.status.code(), Some(75), "{}", said(tried));
    let told = said(tried);
    assert!(
        told.contains("cannot be brought to an end: cannot write 'z.txt'"),
        "{told}"
    );
    assert!(told.contains("alt operation abandon "), "{told}");
}

fn failed_midway(bench: &Bench, command: &[&str], abort: &[&str], go_on: Option<&[&str]>) {
    let repo = bench.repo();
    let [before, done] = begun(bench, command, abort);

    // once it can be written, the next command that writes finishes it,
    // and an undo takes the whole command back
    stuck(bench, command);
    let next = alt(&repo, &["branch", "-d", "no-such-branch"], None);
    assert!(said(&next).contains("not found"), "{}", said(&next));
    assert_eq!(pending(&repo), [""; 0]);
    match go_on {
        None => assert_eq!(bench.shape(), done),
        Some(go_on) => {
            let found = bench.shape();
            run(&repo, go_on, 1);
            assert_eq!(bench.shape(), done);
            restore(&repo, &bench.kept("began"));
            stuck(bench, command);
            alt(&repo, &["branch", "-d", "no-such-branch"], None);
            assert_eq!(bench.shape(), found);
        }
    }
    run(&repo, &["undo"], 0);
    assert_eq!(
        bench.shape(),
        before,
        "the undo of {command:?} brought to an end"
    );

    // while it cannot, every command that writes is refused and says how
    // to go on, the undo among them, and nothing changes
    stuck(bench, command);
    let found = bench.shape();
    for tried in [&["add", "f.txt"][..], &["undo"], &["commit", "-m", "m"]] {
        refused(&limited(&repo, tried, None));
        assert_eq!(bench.shape(), found, "{tried:?}");
    }
    let id = pending(&repo).remove(0);
    refused(&limited(&repo, &["operation", "recover", &id], None));
    assert_eq!(bench.shape(), found);

    // given up, the workspace is as before the command, and no part of
    // the command is left for an undo: that takes back what came before
    let given_up = limited(&repo, &["operation", "abandon", &id], None);
    assert!(given_up.status.success(), "{}", said(&given_up));
    assert_eq!(bench.shape(), before, "{command:?} given up");
    assert_eq!(pending(&repo), [""; 0]);
    run(&repo, &["undo"], 0);
    assert_eq!(
        bench.shape(),
        done,
        "the undo after {command:?} was given up"
    );

    // the abort of the operation gives the command up as well
    stuck(bench, command);
    let aborted = limited(&repo, abort, None);
    assert!(aborted.status.success(), "{}", said(&aborted));
    assert!(said(&aborted).contains("aborted."), "{}", said(&aborted));
    assert_eq!(bench.shape(), before, "{abort:?} of {command:?}");
    run(&repo, command, 1);
    assert_eq!(bench.shape(), done, "{command:?} run again");

    // killed at any point while it is given up, the next command that
    // writes ends it the same way
    stuck(bench, command);
    copy(&repo, &bench.kept("stuck"));
    let list = bench.kept("points");
    let listing = format!("list:{}", list.display());
    let id = pending(&repo).remove(0);
    let abandon = ["operation", "abandon", &id];
    assert!(limited(&repo, &abandon, Some(&listing)).status.success());
    let points = std::fs::read_to_string(&list).unwrap();
    assert!(points.lines().count() > 3, "{points}");
    for (at, point) in points.lines().enumerate() {
        restore(&repo, &bench.kept("stuck"));
        let killed = limited(&repo, &abandon, Some(&(at + 1).to_string()));
        assert_eq!(killed.status.code(), None, "{point}: {}", said(&killed));
        let next = limited(&repo, &["branch", "-d", "no-such-branch"], None);
        let found = bench.shape();
        match found == before {
            true => assert!(
                said(&next).contains("not found"),
                "{point}: {}",
                said(&next)
            ),
            // not yet in the log: as stuck as it was
            false => {
                refused(&next);
                let again = limited(&repo, &abandon, None);
                assert!(again.status.success(), "{point}: {}", said(&again));
                assert_eq!(
                    bench.shape(),
                    before,
                    "given up after it was killed at {point}"
                );
            }
        }
        assert!(alt(&repo, &["fsck"], None).status.success(), "{point}");
    }
}

#[test]
fn a_merge_that_fails_as_it_writes_the_conflict_is_finished_or_given_up_whole() {
    failed_midway(&diverged(), MERGE, &["merge", "--abort"], None);
}

#[test]
fn a_revert_that_fails_as_it_writes_a_commit_is_finished_or_given_up_whole() {
    // the commit of the first revert is made and its files are not all
    // written: finished, the second revert is still to come
    let go_on: &[&str] = &["revert", "--continue"];
    failed_midway(&to_revert(), REVERT, &["revert", "--abort"], Some(go_on));
}

/// Nothing at all is written: not a file, not the index, not a state
/// file, not a record that waits to be recovered.
fn nothing_written(bench: &Bench, command: &[&str], path: &str) {
    let repo = bench.repo();
    let before = bench.shape();
    let log = run(&repo, &["op-log"], 0);
    let tried = alt(&repo, command, None);
    assert!(
        tried.status.code().is_some_and(|code| code != 0),
        "{}",
        said(&tried)
    );
    let why = format!("checkout target '{path}' is a directory holding untracked files");
    assert!(said(&tried).contains(&why), "{command:?}: {}", said(&tried));
    assert_eq!(bench.shape(), before, "{command:?}");
    assert_eq!(pending(&repo), [""; 0], "{command:?}");
    assert_eq!(run(&repo, &["op-log"], 0), log, "{command:?}");
}

#[test]
fn a_directory_that_holds_a_file_refuses_a_merge_a_pick_and_a_revert_before_they_write() {
    let bench = diverged();
    bench.write("new.txt/keep", "mine\n");
    nothing_written(&bench, MERGE, "new.txt");
    nothing_written(&bench, &["cherry-pick", "side"], "new.txt");
    // out of the way, the merge stops
    std::fs::remove_dir_all(bench.repo().join("new.txt")).unwrap();
    run(&bench.repo(), MERGE, 1);

    // the commit before the last changed f.txt and removed new.txt:
    // reverting it stops in f.txt and has new.txt to bring back
    let bench = Bench::new();
    run(&bench.repo(), &["init", "."], 0);
    bench.write("f.txt", "1\n2\n3\n");
    bench.write("new.txt", "new\n");
    bench.commit("files");
    bench.write("f.txt", "1\nb\n3\n");
    std::fs::remove_file(bench.repo().join("new.txt")).unwrap();
    bench.commit("to b");
    bench.write("f.txt", "1\nc\n3\n");
    bench.commit("to c");
    bench.write("new.txt/keep", "mine\n");
    nothing_written(&bench, &["revert", "HEAD~1"], "new.txt");
    std::fs::remove_dir_all(bench.repo().join("new.txt")).unwrap();
    run(&bench.repo(), &["revert", "HEAD~1"], 1);
    assert_eq!(
        std::fs::read(bench.repo().join("new.txt")).unwrap(),
        b"new\n"
    );
}

#[test]
fn a_directory_that_holds_a_file_refuses_the_next_stop_of_a_rebase_before_it_writes() {
    // both commits of `side` stop on `main`; the second adds new.txt
    let bench = Bench::new();
    let repo = bench.repo();
    run(&repo, &["init", "."], 0);
    bench.write("f.txt", "1\n2\n3\n");
    bench.commit("files");
    run(&repo, &["switch", "-c", "side"], 0);
    bench.write("f.txt", "1\nside\n3\n");
    bench.commit("side");
    bench.write("f.txt", "1\nside again\n3\n");
    bench.write("new.txt", "new\n");
    bench.commit("side again");
    run(&repo, &["switch", "main"], 0);
    bench.write("f.txt", "1\nmain\n3\n");
    bench.commit("main");
    run(&repo, &["switch", "side"], 0);
    run(&repo, &["rebase", "main"], 1);
    bench.write("f.txt", "resolved\n");
    run(&repo, &["add", "f.txt"], 0);
    copy(&repo, &bench.kept("resolved"));
    run(&repo, &["rebase", "--continue"], 1);
    let stopped = bench.shape();

    // the step that was resolved is committed; the conflict of the next
    // is not written, and nothing of it waits to be recovered
    restore(&repo, &bench.kept("resolved"));
    bench.write("new.txt/keep", "mine\n");
    let tried = alt(&repo, &["rebase", "--continue"], None);
    assert!(!tried.status.success(), "{}", said(&tried));
    let why = "checkout target 'new.txt' is a directory holding untracked files";
    assert!(said(&tried).contains(why), "{}", said(&tried));
    assert_eq!(pending(&repo), [""; 0]);
    assert_eq!(std::fs::read(repo.join("f.txt")).unwrap(), b"resolved\n");
    assert!(repo.join(".alt/REBASE_STATE").exists());
    std::fs::remove_dir_all(repo.join("new.txt")).unwrap();
    run(&repo, &["rebase", "--continue"], 1);
    assert_eq!(bench.shape(), stopped);
}

#[test]
fn an_undo_that_would_write_into_a_directory_holding_a_file_changes_nothing() {
    let bench = Bench::new();
    let repo = bench.repo();
    run(&repo, &["init", "."], 0);
    for (file, text) in [
        ("f.txt", "1\n2\n3\n"),
        ("f.txt", "1\nb\n3\n"),
        ("f.txt", "1\nc\n3\n"),
    ] {
        bench.write(file, text);
        bench.commit(text);
    }
    bench.write("g.txt", "g\n");
    bench.commit("g");
    let before = bench.shape();
    // the first revert is committed and removes g.txt, the second stops:
    // the undo moves the branch back and has g.txt to write
    run(&repo, &["revert", "HEAD", "HEAD~2"], 1);
    let stopped = bench.shape();
    bench.write("g.txt/keep", "mine\n");
    let found = bench.shape();
    let log = run(&repo, &["op-log"], 0);
    let undone = alt(&repo, &["undo"], None);
    assert!(!undone.status.success(), "{}", said(&undone));
    let why = "checkout target 'g.txt' is a directory holding untracked files";
    assert!(said(&undone).contains(why), "{}", said(&undone));
    assert_eq!(bench.shape(), found);
    assert_eq!(run(&repo, &["op-log"], 0), log);
    assert_eq!(pending(&repo), [""; 0]);
    // as the revert left it, the undo takes all of it back
    std::fs::remove_dir_all(repo.join("g.txt")).unwrap();
    assert_eq!(bench.shape(), stopped);
    run(&repo, &["undo"], 0);
    assert_eq!(bench.shape(), before);
}
