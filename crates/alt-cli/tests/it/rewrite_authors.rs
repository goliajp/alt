//! Whom a commit is by, said on the command line, and a stretch of
//! history made over by one command: `alt commit --author`, `--date`,
//! `--reset-author`, and `alt rebase --exec`, each beside git's.

use std::path::Path;
use std::process::{Command, Output};

use crate::like_git::{Pair, alt, alt_daemon, fails, files, git, heads, index, ok};
use crate::of_late;

const OLD: [(&str, &str); 3] = [
    ("GIT_AUTHOR_NAME", "Old"),
    ("GIT_AUTHOR_EMAIL", "old@x"),
    ("GIT_AUTHOR_DATE", "1600000000 +0200"),
];

/// Commits everything in both as the author [`OLD`].
fn commit_as_old(pair: &Pair, message: &str) {
    pair.both(&["add", "-A"]);
    let mut ours = crate::alt_command();
    ours.current_dir(&pair.alt).env("ALT_NO_DAEMON", "1");
    ours.args(["commit", "-m", message]);
    let mut theirs = Command::new("git");
    theirs
        .current_dir(&pair.git)
        .args(["commit", "-q", "-m", message]);
    theirs
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for command in [&mut ours, &mut theirs] {
        let committer = [
            ("GIT_COMMITTER_NAME", "tester"),
            ("GIT_COMMITTER_EMAIL", "t@e"),
        ];
        command.envs(committer).envs(OLD);
        command.env("GIT_COMMITTER_DATE", "1600000000 +0000");
        ok(command.output().unwrap());
    }
}

/// `main` with two commits and `topic` with two of its own on the first,
/// all by [`OLD`]; `topic` checked out.
fn old_history() -> Pair {
    let pair = Pair::new();
    pair.write("f", "1\n");
    commit_as_old(&pair, "base");
    pair.both(&["branch", "topic"]);
    pair.write("g", "2\n");
    commit_as_old(&pair, "main moves on");
    pair.both(&["switch", "topic"]);
    pair.write("a", "a\n");
    commit_as_old(&pair, "topic one");
    pair.write("b", "b\n");
    commit_as_old(&pair, "topic two");
    pair
}

fn same_commits(pair: &Pair, revs: &[&str]) {
    for rev in revs {
        let ours = ok(alt(&pair.alt, &["rev-parse", rev]));
        assert_eq!(ours, ok(git(&pair.git, &["rev-parse", rev])), "{rev}");
    }
}

fn authors(repo: &Path) -> String {
    ok(alt(repo, &["log", "--pretty=raw", "-n", "4"]))
        .lines()
        .filter(|line| line.starts_with("author "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The command that makes the commit just replayed the caller's own, for
/// `alt` and for git.
fn made_mine() -> (String, &'static str) {
    let ours = format!(
        "'{}' commit --amend --reset-author --no-edit",
        env!("CARGO_BIN_EXE_alt")
    );
    (ours, "git commit -q --amend --reset-author --no-edit")
}

fn rebased(pair: &Pair, ours: &[&str], theirs: &[&str]) -> (Output, Output) {
    (alt(&pair.alt, ours), git(&pair.git, theirs))
}

#[test]
fn a_commit_is_by_whom_and_of_when_the_command_says() {
    let pair = old_history();
    pair.write("c", "c\n");
    pair.both(&["add", "c"]);
    for args in [
        &[
            "commit",
            "-m",
            "named",
            "--author=A B <ab@x>",
            "--date=1234567890 -0500",
        ][..],
        &[
            "commit",
            "--amend",
            "--no-edit",
            "--author=  New  <  new@x >",
        ],
        &[
            "commit",
            "--amend",
            "--no-edit",
            "--date=2020-01-02T03:04:05 +0100",
        ],
        &["commit", "--amend", "-m", "again", "--reset-author"],
        &[
            "commit",
            "--amend",
            "--no-edit",
            "--reset-author",
            "--date=1650000000 +0100",
        ],
        // a text an earlier author holds names that author
        &["commit", "--amend", "--no-edit", "--author=old@"],
        &[
            "commit",
            "--allow-empty",
            "-m",
            "dated",
            "--date=1400000000 +0000",
        ],
    ] {
        pair.both(args);
        same_commits(&pair, &["HEAD"]);
    }
    let before = heads(&pair.alt);
    for (args, said) in [
        (
            &[
                "commit",
                "--amend",
                "--no-edit",
                "--reset-author",
                "--author=X <x@y>",
            ][..],
            "fatal: options '--reset-author' and '--author' cannot be used together\n",
        ),
        (
            &["commit", "--allow-empty", "-m", "x", "--reset-author"],
            "fatal: --reset-author can be used only with -C, -c or --amend.\n",
        ),
        (
            &["commit", "--allow-empty", "-m", "x", "--author=nobody here"],
            "fatal: --author 'nobody here' is not 'Name <email>' and matches no existing author\n",
        ),
        (
            &["commit", "--allow-empty", "-m", "x", "--author=<a@b>"],
            "fatal: empty ident name (for <a@b>) not allowed\n",
        ),
        (
            &["commit", "--allow-empty", "-m", "x", "--date=garbage"],
            "fatal: invalid date format: garbage\n",
        ),
    ] {
        of_late::same_failure(&pair, args, 128, said);
    }
    assert_eq!(heads(&pair.alt), before);
    // the daemon makes the commit the command itself makes
    let args = [
        "commit",
        "--amend",
        "--allow-empty",
        "--no-edit",
        "--author=D <d@x>",
        "--date=1000000000 +0000",
    ];
    ok(alt_daemon(&pair.alt, &args));
    ok(git(&pair.git, &args));
    same_commits(&pair, &["HEAD"]);
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(heads(&pair.alt), before);
}

#[test]
fn a_rebase_runs_a_command_after_each_commit_and_makes_them_the_callers() {
    let (ours, theirs) = made_mine();
    for onto in ["main", "main~1"] {
        let pair = old_history();
        let (a, g) = rebased(
            &pair,
            &["rebase", onto, "-x", &ours],
            &["rebase", onto, "-x", theirs],
        );
        let told = String::from_utf8_lossy(&a.stderr).into_owned();
        assert_eq!(told.matches("Executing: ").count(), 2, "{told}");
        assert!(ok(a).ends_with("Successfully rebased and updated refs/heads/topic.\n"));
        ok(g);
        same_commits(&pair, &["HEAD", "HEAD~1", "HEAD~2"]);
        pair.assert_same_state();
        let by = authors(&pair.alt);
        let mine = "author tester <t@e> 1700000000 +0000";
        assert!(
            by.starts_with(&format!("{mine}\n{mine}\nauthor Old <old@x>")),
            "{by}"
        );
        assert!(!ok(alt(&pair.alt, &["status", "--json"])).contains("rebase"));
    }
}

#[test]
fn the_daemon_and_a_parallel_workspace_run_the_commands_of_a_rebase() {
    let (ours, theirs) = made_mine();
    let pair = old_history();
    let served = alt_daemon(
        &pair.alt,
        &["rebase", "main", "-x", &ours, "-x", "echo said"],
    );
    assert_eq!(
        String::from_utf8_lossy(&served.stdout)
            .matches("said\n")
            .count(),
        2
    );
    ok(served);
    ok(git(
        &pair.git,
        &["rebase", "main", "-x", theirs, "-x", "true"],
    ));
    same_commits(&pair, &["HEAD", "HEAD~1"]);
    // with --json the output is the result of the rebase alone
    let pair = old_history();
    let json = alt(&pair.alt, &["rebase", "main", "--json", "-x", "echo said"]);
    assert!(String::from_utf8_lossy(&json.stderr).contains("said\n"));
    let json = ok(json);
    assert!(
        json.starts_with('{') && json.contains("\"branch\":\"topic\""),
        "{json}"
    );
    assert_eq!(json.lines().count(), 1, "{json}");

    let pair = old_history();
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(&pair.alt, &["switch", "main"]));
    ok(git(&pair.git, &["switch", "-q", "main"]));
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap()],
    ));
    ok(alt(&tree, &["switch", "topic"]));
    let (mine, staged) = (files(&pair.alt), index(&pair.alt));
    ok(alt(&tree, &["rebase", "main", "-x", &ours]));
    ok(git(&pair.git, &["switch", "-q", "topic"]));
    ok(git(&pair.git, &["rebase", "main", "-x", theirs]));
    same_commits(&pair, &["topic", "topic~1"]);
    assert_eq!((files(&pair.alt), index(&pair.alt)), (mine, staged));
    assert_eq!(std::fs::read_to_string(tree.join("g")).unwrap(), "2\n");
}

#[test]
fn a_command_that_fails_stops_the_rebase_where_it_can_go_on() {
    let pair = old_history();
    let before = heads(&pair.alt);
    let (a, g) = pair.run(&["rebase", "main", "-x", "test -f b"]);
    let said = fails(a, 1);
    assert!(said.contains("Executing: test -f b\nwarning: execution failed: test -f b\n"));
    assert!(said.contains("\n  alt rebase --continue\n"), "{said}");
    assert!(fails(g, 1).contains("warning: execution failed: test -f b\n"));
    // the commit the command followed is made, and the rebase waits
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "a\n");
    assert!(!pair.alt.join("b").exists());
    assert!(ok(alt(&pair.alt, &["status"])).contains("rebase"));
    // gone on with, the command that failed is not run again
    pair.both(&["rebase", "--continue"]);
    same_commits(&pair, &["HEAD", "HEAD~1"]);
    pair.assert_same_state();

    // given up, the branch is where it was
    let pair = old_history();
    fails(alt(&pair.alt, &["rebase", "main", "-x", "false"]), 1);
    ok(alt(&pair.alt, &["rebase", "--abort"]));
    assert_eq!(heads(&pair.alt), before);
    assert!(!pair.alt.join("g").exists());

    // a command that leaves a change to a tracked file stops it too
    let (a, g) = pair.run(&["rebase", "main", "-x", "echo x >>a"]);
    let said = fails(a, 1);
    assert!(said.contains("error: execution succeeded: echo x >>a\nbut left changes to"));
    assert!(fails(g, 1).contains("execution succeeded: echo x >>a\nbut left changes to"));
    for root in [&pair.alt, &pair.git] {
        std::fs::write(root.join("a"), "a\n").unwrap();
    }
    let (a, g) = pair.run(&["rebase", "--continue"]);
    fails(a, 1);
    fails(g, 1);
    for root in [&pair.alt, &pair.git] {
        std::fs::write(root.join("a"), "a\n").unwrap();
    }
    pair.both(&["rebase", "--continue"]);
    same_commits(&pair, &["HEAD", "HEAD~1"]);

    for (args, said) in [
        (
            &["rebase", "main", "-x", ""][..],
            "error: empty exec command\n",
        ),
        (
            &["rebase", "main", "-x", "a\nb"],
            "error: exec commands cannot contain newlines\n",
        ),
    ] {
        of_late::same_failure(&pair, args, 1, said);
    }
}

/// Each stretch of a rebase between two of its commands is an operation
/// of its own, as each command is: an undo takes back the last stretch,
/// and the rebase it leaves under way is given up for the branch as it
/// was.
#[test]
fn a_rebase_with_commands_is_undone_back_into_and_given_up() {
    let (ours, _) = made_mine();
    let pair = old_history();
    let state = |pair: &Pair| (files(&pair.alt), authors(&pair.alt));
    let before = (heads(&pair.alt), state(&pair));
    ok(alt(&pair.alt, &["rebase", "main", "-x", &ours]));
    let rebased = state(&pair);
    assert_ne!(heads(&pair.alt), before.0);
    ok(alt(&pair.alt, &["undo"]));
    assert!(ok(alt(&pair.alt, &["status"])).contains("rebase"));
    assert_eq!(state(&pair), rebased);
    ok(alt(&pair.alt, &["rebase", "--abort"]));
    assert_eq!((heads(&pair.alt), state(&pair)), before);
}

#[test]
fn a_rebase_gives_the_commits_it_replays_the_date_it_is_run_at() {
    let pair = old_history();
    ok(alt(&pair.alt, &["rebase", "main~1", "--reset-author-date"]));
    ok(git(
        &pair.git,
        &["rebase", "-q", "main~1", "--reset-author-date"],
    ));
    let dated = |text: String| -> Vec<(String, bool)> {
        let authors = text.lines().filter(|line| line.starts_with("author "));
        let split = authors.map(|line| {
            let mut words = line.rsplitn(3, ' ');
            let (_, when, who) = (words.next(), words.next().unwrap(), words.next().unwrap());
            (who.to_owned(), when == "1600000000")
        });
        split.collect()
    };
    let ours = dated(ok(alt(&pair.alt, &["log", "--pretty=raw", "-n", "3"])));
    let theirs = dated(ok(git(&pair.git, &["log", "--pretty=raw", "-n", "3"])));
    assert_eq!(ours, theirs);
    let old = "author Old <old@x>".to_owned();
    assert_eq!(
        ours,
        [(old.clone(), false), (old.clone(), false), (old, true)]
    );
    // the two are run at moments of their own: the commits differ by them
    assert_eq!(index(&pair.alt), ok(git(&pair.git, &["ls-files", "-s"])));
    assert_eq!(files(&pair.alt), files(&pair.git));
}
