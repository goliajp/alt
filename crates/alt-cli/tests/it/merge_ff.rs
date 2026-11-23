//! `alt merge --no-ff` and `--ff-only`, and the merge commits every
//! `alt flow … finish` makes, each beside what git makes of the same
//! history.

use crate::like_git::{Pair, alt, alt_daemon, export, fails, git, heads, ok};

/// A base commit on `main`, then `topic` one commit ahead of it; both
/// are left on `main`.
fn ahead(topic: &str) -> Pair {
    let pair = Pair::new();
    pair.write("a.txt", "one\n");
    pair.commit_all("base");
    pair.both(&["switch", "-c", topic]);
    pair.write("b.txt", "two\n");
    pair.commit_all("advance");
    pair.both(&["switch", "main"]);
    pair
}

#[test]
fn no_ff_records_a_merge_commit_where_git_does() {
    let pair = ahead("feat");
    let said = ok(alt(&pair.alt, &["merge", "--no-ff", "feat"]));
    let git_said = ok(git(&pair.git, &["merge", "--no-ff", "feat"]));
    assert_eq!(said.lines().next(), git_said.lines().next());
    pair.assert_same_state();
    let out = export(&pair.alt);
    let made = ok(git(
        &out.path().join("exported"),
        &["log", "-1", "--format=%s|%P"],
    ));
    assert_eq!(made, ok(git(&pair.git, &["log", "-1", "--format=%s|%P"])));
    assert!(made.starts_with("Merge branch 'feat'|"), "{made}");
    let parents = made.trim_end().split('|').nth(1).unwrap();
    assert_eq!(parents.split(' ').count(), 2, "two parents: {made}");
}

#[test]
fn a_merge_into_another_branch_than_main_names_it() {
    let pair = ahead("feat");
    pair.both(&["switch", "-c", "trunk"]);
    pair.both(&["merge", "--no-ff", "feat"]);
    pair.assert_same_state();
    let out = export(&pair.alt);
    let subject = ok(git(
        &out.path().join("exported"),
        &["log", "-1", "--format=%s"],
    ));
    assert_eq!(subject, "Merge branch 'feat' into trunk\n");
}

#[test]
fn no_ff_changes_nothing_where_the_branch_has_it_all() {
    let pair = ahead("feat");
    pair.both(&["merge", "feat"]);
    assert_eq!(
        pair.same(&["merge", "--no-ff", "feat"]),
        "Already up to date.\n"
    );
    pair.assert_same_state();
}

#[test]
fn ff_only_fast_forwards_or_refuses() {
    let pair = ahead("feat");
    pair.both(&["merge", "--ff-only", "feat"]);
    pair.assert_same_state();

    // both sides move on: no fast-forward is left
    pair.write("c.txt", "main\n");
    pair.commit_all("on main");
    pair.both(&["switch", "feat"]);
    pair.write("d.txt", "feat\n");
    pair.commit_all("on feat");
    pair.both(&["switch", "main"]);
    let before = heads(&pair.alt);
    let said = fails(alt(&pair.alt, &["merge", "--ff-only", "feat"]), 128);
    let git_said = fails(git(&pair.git, &["merge", "--ff-only", "feat"]), 128);
    let refused = "fatal: Not possible to fast-forward, aborting.";
    assert!(said.lines().any(|line| line == refused), "{said}");
    assert_eq!(git_said.lines().last(), Some(refused));
    assert_eq!(heads(&pair.alt), before);
    pair.assert_same_state();
}

#[test]
fn no_ff_and_ff_only_together_are_refused() {
    let pair = ahead("feat");
    fails(
        alt(&pair.alt, &["merge", "--no-ff", "--ff-only", "feat"]),
        2,
    );
}

#[test]
fn no_ff_through_the_daemon_and_undone() {
    let pair = ahead("feat");
    let before = heads(&pair.alt);
    ok(alt_daemon(&pair.alt, &["merge", "--no-ff", "feat"]));
    ok(git(&pair.git, &["merge", "--no-ff", "feat"]));
    pair.assert_same_state();
    ok(alt_daemon(&pair.alt, &["undo"]));
    assert_eq!(heads(&pair.alt), before);
    assert!(!pair.alt.join("b.txt").exists());
}

/// The history a flow works on, in both: `main` with one commit and
/// `develop` at it.
fn flow_pair() -> Pair {
    let pair = Pair::new();
    pair.write("app.txt", "v1\n");
    pair.commit_all("initial");
    ok(alt(&pair.alt, &["flow", "init"]));
    ok(git(&pair.git, &["switch", "-q", "-c", "develop"]));
    pair
}

#[test]
fn feature_finish_makes_the_merge_commit_git_makes_without_fast_forward() {
    let pair = flow_pair();
    ok(alt(&pair.alt, &["flow", "feature", "start", "login"]));
    ok(git(&pair.git, &["switch", "-q", "-c", "feature/login"]));
    pair.write("login.txt", "login\n");
    pair.commit_all("add login");
    let before = heads(&pair.alt);

    ok(alt(&pair.alt, &["flow", "feature", "finish", "login"]));
    ok(git(&pair.git, &["switch", "-q", "develop"]));
    ok(git(&pair.git, &["merge", "-q", "--no-ff", "feature/login"]));
    ok(git(&pair.git, &["branch", "-q", "-d", "feature/login"]));
    pair.assert_same_state();
    let subject = ok(git(&pair.git, &["log", "-1", "--format=%s"]));
    assert_eq!(subject, "Merge branch 'feature/login' into develop\n");

    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(heads(&pair.alt), before);
}

#[test]
fn a_feature_without_commits_finishes_without_a_merge_commit() {
    let pair = flow_pair();
    ok(alt(&pair.alt, &["flow", "feature", "start", "idle"]));
    ok(alt(&pair.alt, &["flow", "feature", "finish", "idle"]));
    pair.assert_same_state();
}

/// A release or a hotfix finished in alt beside the merges git-flow
/// makes: the topic into `main`, then `main` into `develop`, neither by
/// fast-forward.
fn finishes_like_git(kind: &str, base: &str) {
    let pair = flow_pair();
    let topic = format!("{kind}/1.0");
    ok(alt(&pair.alt, &["flow", kind, "start", "1.0"]));
    ok(git(&pair.git, &["switch", "-q", "-c", &topic, base]));
    pair.write("version.txt", "1.0\n");
    pair.commit_all("bump version");
    let before = heads(&pair.alt);

    ok(alt(&pair.alt, &["flow", kind, "finish", "1.0"]));
    ok(git(&pair.git, &["switch", "-q", "main"]));
    ok(git(&pair.git, &["merge", "-q", "--no-ff", &topic]));
    ok(git(&pair.git, &["switch", "-q", "develop"]));
    ok(git(&pair.git, &["merge", "-q", "--no-ff", "main"]));
    ok(git(&pair.git, &["branch", "-q", "-D", &topic]));
    pair.assert_same_state();
    let subject = |branch: &str| ok(git(&pair.git, &["log", "-1", "--format=%s", branch]));
    assert_eq!(subject("develop"), "Merge branch 'main' into develop\n");
    assert_eq!(subject("main"), format!("Merge branch '{topic}'\n"));

    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(heads(&pair.alt), before);
}

#[test]
fn release_finish_makes_two_merge_commits() {
    finishes_like_git("release", "develop");
}

#[test]
fn hotfix_finish_makes_two_merge_commits() {
    finishes_like_git("hotfix", "main");
}
