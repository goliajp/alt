//! `alt log` of a range and of paths, and `--oneline`, beside git.

use crate::like_git::{Pair, alt, alt_daemon, fails, git, ok};

/// `main` with three commits, `side` from the first with two of its own,
/// merged into `main`, and one commit after the merge. Each commit is a
/// second later than the one before it in this list:
///
/// ```text
/// c1 - c2 - c3 ---- merge - c4      main
///   \- s1 - s2 ----/                side
/// ```
fn history() -> Pair {
    let pair = Pair::new();
    let mut time = 1_700_000_000;
    let mut commit = |pair: &Pair, message: &str| {
        time += 1;
        pair.commit_all_at(message, time);
    };
    pair.write("a.txt", "one\n");
    pair.write("d/x", "x\n");
    commit(&pair, "c1");
    pair.both(&["branch", "side"]);
    pair.write("a.txt", "two\n");
    commit(&pair, "c2");
    pair.both(&["switch", "side"]);
    pair.write("d/y", "y\n");
    commit(&pair, "s1");
    pair.write("d/x", "x2\n");
    commit(&pair, "s2");
    pair.both(&["switch", "main"]);
    pair.write("b.txt", "b\n");
    commit(&pair, "c3");
    pair.merge_at("side", 1_700_000_010);
    pair.write("a.txt", "three\n");
    pair.commit_all_at("c4", 1_700_000_011);
    pair
}

fn subjects(said: &str) -> Vec<&str> {
    let subject = |line| str::split_once(line, ' ').unwrap().1;
    said.lines().map(subject).collect()
}

#[test]
fn oneline_is_the_abbreviated_id_and_the_subject() {
    let pair = history();
    let said = pair.same(&["log", "--oneline"]);
    assert_eq!(
        subjects(&said),
        ["c4", "Merge branch 'side'", "c3", "s2", "s1", "c2", "c1"]
    );
    assert!(said.lines().all(|line| line.find(' ') == Some(7)), "{said}");
    assert_eq!(
        pair.same(&["log", "--oneline", "-n", "2"]).lines().count(),
        2
    );
    pair.same(&["log", "--oneline", "-3", "side"]);
    // the full ids with --pretty, as before
    pair.same(&["log", "--pretty=oneline", "side"]);
}

#[test]
fn two_dots_leave_what_leads_to_the_first() {
    let pair = history();
    for range in [
        "main~2..main",
        "side..main",
        "main..side",
        "side~1..side",
        "..side",
        "side..",
    ] {
        pair.same(&["log", "--oneline", range]);
    }
    let said = pair.same(&["log", "--oneline", "side..main"]);
    assert_eq!(subjects(&said), ["c4", "Merge branch 'side'", "c3", "c2"]);
    assert_eq!(pair.same(&["log", "--oneline", "main..side"]), "");
    assert_eq!(
        pair.same(&["log", "--oneline", "-n", "2", "side..main"])
            .lines()
            .count(),
        2
    );
    pair.same(&["log", "--pretty=raw", "main~1^2~1..main~1"]);
}

#[test]
fn three_dots_leave_what_leads_to_both() {
    let pair = history();
    let said = pair.same(&["log", "--oneline", "side...main~2"]);
    assert_eq!(subjects(&said), ["c3", "s2", "s1", "c2"]);
    pair.same(&["log", "--oneline", "main~2...side"]);
    assert_eq!(pair.same(&["log", "--oneline", "main...main"]), "");
    pair.same(&["log", "--oneline", "side...main"]);
}

#[test]
fn paths_leave_the_commits_that_change_them() {
    let pair = history();
    let said = pair.same(&["log", "--oneline", "--", "a.txt"]);
    assert_eq!(subjects(&said), ["c4", "c2", "c1"]);
    // a merge that has a path as one parent has it is passed over
    let said = pair.same(&["log", "--oneline", "--", "d"]);
    assert_eq!(subjects(&said), ["s2", "s1", "c1"]);
    pair.same(&["log", "--oneline", "--", "d/x", "b.txt"]);
    pair.same(&["log", "--oneline", "side..main", "--", "a.txt"]);
    pair.same(&["log", "--oneline", "main~3..main", "--", "d"]);
    pair.same(&["log", "--oneline", "-n", "1", "side", "--", "d/x"]);
    assert_eq!(pair.same(&["log", "--oneline", "--", "nope"]), "");
    // from the directory the command runs in
    let said = ok(alt(
        &pair.alt.join("d"),
        &["log", "--oneline", "--", "x", "../b.txt"],
    ));
    assert_eq!(
        said,
        ok(git(
            &pair.git.join("d"),
            &["log", "--oneline", "--", "x", "../b.txt"]
        ))
    );
    // the merge has the two paths as neither of its parents has them
    assert_eq!(subjects(&said), ["Merge branch 'side'", "c3", "s2", "c1"]);
}

#[test]
fn a_range_holds_among_commits_of_one_date() {
    // the dates do not tell which commit came first: R - A1 - A2 - A and
    // R - B, where A..B is B alone
    let pair = Pair::new();
    pair.write("r", "r\n");
    pair.commit_all("R");
    pair.both(&["branch", "b"]);
    for name in ["A1", "A2", "A"] {
        pair.write("a", name);
        pair.commit_all(name);
    }
    pair.both(&["switch", "b"]);
    pair.write("b", "b\n");
    pair.commit_all("B");
    let said = pair.same(&["log", "--oneline", "main..b"]);
    assert_eq!(subjects(&said), ["B"]);
    pair.same(&["log", "--oneline", "b..main"]);
    pair.same(&["log", "--oneline", "b...main"]);
}

#[test]
fn a_range_of_what_names_nothing_is_refused() {
    let pair = history();
    let said = fails(alt(&pair.alt, &["log", "--oneline", "nosuch..main"]), 128);
    assert!(
        said.starts_with("fatal: ambiguous argument 'nosuch..main': unknown revision"),
        "{said}"
    );
    fails(git(&pair.git, &["log", "--oneline", "nosuch..main"]), 128);
}

#[test]
fn the_json_and_the_daemon_list_the_same_commits() {
    let pair = history();
    let args = ["log", "--oneline", "side..main", "--", "a.txt"];
    assert_eq!(ok(alt_daemon(&pair.alt, &args)), ok(alt(&pair.alt, &args)));
    let json = ok(alt(
        &pair.alt,
        &["log", "--json", "side..main", "--", "a.txt"],
    ));
    let listed = ok(git(
        &pair.git,
        &["log", "--format=%H", "side..main", "--", "a.txt"],
    ));
    let mut at = 0;
    for oid in listed.lines() {
        let found = json[at..].find(&format!("\"oid\":\"{oid}\""));
        at += found.unwrap_or_else(|| panic!("{oid} in order in {json}"));
    }
    assert_eq!(json.matches("\"oid\":").count(), listed.lines().count());
}

#[test]
fn a_parallel_workspace_means_its_own_head() {
    let pair = history();
    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree.to_str().unwrap(), "side"],
    ));
    let said = ok(alt(&tree, &["log", "--oneline", "HEAD~1..HEAD"]));
    assert_eq!(subjects(&said), ["s2"]);
    let said = ok(alt(&tree, &["log", "--oneline", "..main", "--", "a.txt"]));
    assert_eq!(
        said,
        ok(git(
            &pair.git,
            &["log", "--oneline", "side..main", "--", "a.txt"]
        ))
    );
}

/// `a` and `b` each merged the other's first commit, so that the two
/// have two merge bases, and each went on by one commit:
///
/// ```text
/// c0 - a1 - A+B - a2      a
///    X    X
///    \ b1 - B+A - b2      b
/// ```
fn crossed() -> Pair {
    let pair = Pair::new();
    let mut time = 1_700_000_000;
    let mut tick = || {
        time += 1;
        time
    };
    pair.write("base", "base\n");
    pair.commit_all_at("c0", tick());
    pair.both(&["branch", "a"]);
    pair.both(&["branch", "b"]);
    for (branch, old) in [("a", "a-old"), ("b", "b-old")] {
        pair.both(&["switch", branch]);
        pair.write(branch, "one\n");
        pair.commit_all_at(&format!("{branch}1"), tick());
        pair.both(&["branch", old]);
    }
    for (branch, other) in [("a", "b-old"), ("b", "a-old")] {
        pair.both(&["switch", branch]);
        pair.merge_at(other, tick());
        pair.write(branch, "two\n");
        pair.commit_all_at(&format!("{branch}2"), tick());
    }
    pair
}

#[test]
fn a_range_over_a_history_with_two_merge_bases_is_git_s() {
    let pair = crossed();
    let three = pair.same(&["log", "--oneline", "a...b"]);
    assert_eq!(three.lines().count(), 4, "{three}");
    let two = pair.same(&["log", "--oneline", "a..b"]);
    assert_eq!(two.lines().count(), 2, "{two}");
    assert_eq!(pair.same(&["log", "--oneline", "b..a"]).lines().count(), 2);
    pair.same(&["log", "--oneline", "a-old...b", "--", "a"]);
}
