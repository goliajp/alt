//! `merge.ff`, `merge.log` and `merge.suppressDest`: a merge reads them
//! as git's does, and what the command line says goes before them.

use std::process::{Command, Output};

use crate::like_git::{Pair, SIGNING, ok};

const AT: u64 = 1_700_001_000;

/// `merge` with `args` in both, a commit it makes dated `AT`.
fn merge(pair: &Pair, args: &[&str]) -> (Output, Output) {
    let date = format!("{AT} +0000");
    let dated = [("GIT_AUTHOR_DATE", &date), ("GIT_COMMITTER_DATE", &date)];
    let ours = crate::alt_command()
        .current_dir(&pair.alt)
        .env("ALT_NO_DAEMON", "1")
        .env("ALT_PROTECTED_OVERRIDE", "1")
        .envs(SIGNING)
        .envs(dated)
        .arg("merge")
        .args(args)
        .output()
        .unwrap();
    let theirs = Command::new("git")
        .current_dir(&pair.git)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .envs(SIGNING)
        .envs(dated)
        .args(["merge", "--no-edit"])
        .args(args)
        .output()
        .unwrap();
    (ours, theirs)
}

/// `main` with a commit of its own, `side` with three from the commit
/// before it, `behind` at that commit, and `dev` beside `main`.
fn branches() -> Pair {
    let pair = Pair::new();
    pair.write("a", "a\n");
    pair.commit_all_at("base", 1_700_000_000);
    pair.both(&["branch", "behind"]);
    pair.both(&["switch", "-c", "side"]);
    for (n, subject) in [
        "side one",
        "side two\nstill the subject\n\nbody",
        "side three",
    ]
    .iter()
    .enumerate()
    {
        pair.write(&format!("s{n}"), "s\n");
        pair.commit_all_at(subject, 1_700_000_100 + n as u64);
    }
    pair.both(&["tag", "-a", "-m", "tagged", "v1"]);
    pair.both(&["switch", "main"]);
    pair.write("m", "m\n");
    pair.commit_all_at("main", 1_700_000_200);
    pair.both(&["branch", "dev"]);
    pair
}

/// The merge is made alike in both, and taken back.
fn same_merge(pair: &Pair, on: &str, set: &[(&str, &str)], args: &[&str]) {
    pair.both(&["switch", on]);
    for (key, value) in set {
        pair.both(&["config", "--add", key, value]);
    }
    let (ours, theirs) = merge(pair, args);
    let said = String::from_utf8_lossy(&ours.stderr).into_owned();
    assert_eq!(
        ours.status.code(),
        theirs.status.code(),
        "{on} {set:?} {args:?}: {said}"
    );
    pair.assert_same_state();
    if ours.status.code() == Some(128) {
        let theirs = String::from_utf8_lossy(&theirs.stderr).into_owned();
        let last = theirs.lines().last().unwrap_or_default();
        assert!(said.contains(last), "{said}\n{theirs}");
    }
    let before = match on {
        "behind" => "main~1",
        _ => "main",
    };
    pair.both(&["reset", "-q", "--hard", before]);
    if let Some((key, _)) = set.first() {
        ok(crate::like_git::alt(
            &pair.alt,
            &["config", "--unset-all", key],
        ));
        ok(crate::like_git::git(
            &pair.git,
            &["config", "--unset-all", key],
        ));
    }
}

#[test]
fn a_merge_fast_forwards_as_the_setting_says_unless_the_command_line_does() {
    let pair = branches();
    for value in ["false", "true", "only", "bogus"] {
        let set = [("merge.ff", value)];
        same_merge(&pair, "behind", &set, &["side"]);
        same_merge(&pair, "main", &set, &["side"]);
        for flag in ["--ff", "--no-ff", "--ff-only"] {
            same_merge(&pair, "behind", &set, &[flag, "side"]);
            same_merge(&pair, "main", &set, &[flag, "side"]);
        }
    }
}

#[test]
fn the_message_of_a_merge_names_the_branch_merged_into_as_the_setting_says() {
    let pair = branches();
    let unnamed = "merge.suppressDest";
    for (on, set) in [
        ("dev", vec![]),
        ("main", vec![]),
        ("dev", vec![(unnamed, "dev")]),
        ("dev", vec![(unnamed, "d*")]),
        ("dev", vec![(unnamed, "x"), (unnamed, "de?")]),
        ("main", vec![(unnamed, "x")]),
        ("main", vec![(unnamed, "")]),
        ("main", vec![(unnamed, "main"), (unnamed, "")]),
        ("main", vec![(unnamed, ""), (unnamed, "main")]),
    ] {
        same_merge(&pair, on, &set, &["side"]);
    }
}

#[test]
fn the_message_of_a_merge_lists_what_it_brings_where_the_setting_says() {
    let pair = branches();
    let log = "merge.log";
    for value in ["true", "false", "0", "1", "2", "3", "20"] {
        for brought in ["side", "v1", "side~0", "side~1"] {
            same_merge(&pair, "main", &[(log, value)], &["--no-ff", brought]);
        }
        same_merge(&pair, "behind", &[(log, value)], &["--no-ff", "side"]);
        same_merge(&pair, "dev", &[(log, value)], &["side"]);
    }
}
