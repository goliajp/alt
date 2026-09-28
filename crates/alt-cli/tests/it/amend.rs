//! `alt commit --amend`: a new commit replaces the branch tip, as in git —
//! same parents and author, new or kept message, the index's tree — and the
//! old commit stays reachable through the op log.

use std::path::Path;
use std::process::{Command, Output};

fn alt_as(repo: &Path, who: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", who)
        .env("GIT_AUTHOR_EMAIL", format!("{who}@e"))
        .args(args)
        .output()
        .unwrap()
}

fn alt(repo: &Path, args: &[&str]) -> Output {
    alt_as(repo, "tester", args)
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "tester@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "tester@e")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn refused(o: Output) -> String {
    assert!(
        !o.status.success(),
        "expected a refusal: {}",
        String::from_utf8_lossy(&o.stdout)
    );
    String::from_utf8(o.stderr).unwrap()
}

/// The parts of a commit that do not depend on when or where it was made.
#[derive(Debug, PartialEq)]
struct Shape {
    tree: String,
    parents: usize,
    message: String,
}

fn shape(cat_file: &str) -> Shape {
    let (head, message) = cat_file.split_once("\n\n").unwrap();
    Shape {
        tree: head.lines().next().unwrap().to_owned(),
        parents: head.lines().filter(|l| l.starts_with("parent ")).count(),
        message: message.to_owned(),
    }
}

fn field<'a>(cat_file: &'a str, name: &str) -> &'a str {
    cat_file
        .lines()
        .find_map(|l| l.strip_prefix(name))
        .unwrap()
        .rsplitn(3, ' ')
        .nth(2)
        .unwrap()
}

fn write(root: &Path, name: &str, body: &str) {
    std::fs::write(root.join(name), body).unwrap();
}

#[test]
fn amending_matches_git() {
    let (a, g) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, g) = (a.path(), g.path());
    ok(alt(a, &["init", "."]));
    ok(git(g, &["init", "-q", "-b", "main"]));
    for (root, run) in [
        (
            a,
            &(|args: &[&str]| alt(a, args)) as &dyn Fn(&[&str]) -> Output,
        ),
        (g, &|args: &[&str]| git(g, args)),
    ] {
        write(root, "a.txt", "a\n");
        ok(run(&["add", "."]));
        ok(run(&["commit", "-m", "first"]));
        write(root, "b.txt", "b\n");
        ok(run(&["add", "."]));
        ok(run(&["commit", "-m", "second"]));
        // reword only
        ok(run(&["commit", "--amend", "-m", "second, reworded"]));
        // then fold a forgotten file in, keeping the message
        write(root, "c.txt", "c\n");
        ok(run(&["add", "."]));
        ok(run(&["commit", "--amend", "--no-edit"]));
    }
    let alt_head = ok(alt(a, &["cat-file", "-p", "main"]));
    let git_head = ok(git(g, &["cat-file", "-p", "HEAD"]));
    assert_eq!(shape(&alt_head), shape(&git_head));
    assert_eq!(shape(&alt_head).message, "second, reworded\n");
    // the tip still sits directly on "first", not on the replaced commits
    let alt_parent = ok(alt(a, &["cat-file", "-p", "main~1"]));
    let git_parent = ok(git(g, &["cat-file", "-p", "HEAD~1"]));
    assert_eq!(shape(&alt_parent), shape(&git_parent));
    assert_eq!(shape(&alt_parent).message, "first\n");
}

#[test]
fn amend_keeps_the_author_and_takes_a_new_committer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    write(root, "a.txt", "a\n");
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));
    ok(alt_as(
        root,
        "fixer",
        &["commit", "--amend", "-m", "first, fixed"],
    ));
    let head = ok(alt(root, &["cat-file", "-p", "main"]));
    assert_eq!(field(&head, "author "), "tester <tester@e>");
    assert_eq!(field(&head, "committer "), "fixer <fixer@e>");
}

#[test]
fn the_replaced_commit_is_in_the_op_log_and_undo_restores_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    write(root, "a.txt", "a\n");
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));
    let before = ok(alt(root, &["rev-parse", "main"])).trim().to_owned();
    ok(alt(root, &["commit", "--amend", "-m", "first, fixed"]));
    let after = ok(alt(root, &["rev-parse", "main"])).trim().to_owned();
    assert_ne!(before, after);

    let log = ok(alt(root, &["op-log"]));
    assert!(log.contains("verb=amend"), "{log}");
    assert!(log.contains(&format!("{before} -> {after}")), "{log}");

    ok(alt(root, &["undo"]));
    assert_eq!(ok(alt(root, &["rev-parse", "main"])).trim(), before);
}

#[test]
fn amending_a_merge_keeps_both_parents() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    write(root, "a.txt", "a\n");
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    ok(alt(root, &["branch", "side"]));
    write(root, "m.txt", "m\n");
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "on main"]));
    ok(alt(root, &["switch", "side"]));
    write(root, "s.txt", "s\n");
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "on side"]));
    ok(alt(root, &["switch", "main"]));
    ok(alt(root, &["merge", "side"]));

    ok(alt(
        root,
        &["commit", "--amend", "-m", "merge side, reworded"],
    ));
    let head = ok(alt(root, &["cat-file", "-p", "main"]));
    assert_eq!(shape(&head).parents, 2, "{head}");
    assert_eq!(shape(&head).message, "merge side, reworded\n");
}

#[test]
fn amend_is_refused_where_git_refuses_it_and_on_protected_branches() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    write(root, "a.txt", "a\n");
    ok(alt(root, &["add", "."]));
    let err = refused(alt(root, &["commit", "--amend", "-m", "x"]));
    assert!(err.contains("no commit to amend"), "{err}");
    ok(alt(root, &["commit", "-m", "base"]));

    // mid-merge
    ok(alt(root, &["branch", "side"]));
    write(root, "a.txt", "main\n");
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "main"]));
    ok(alt(root, &["switch", "side"]));
    write(root, "a.txt", "side\n");
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "side"]));
    ok(alt(root, &["switch", "main"]));
    assert!(!alt(root, &["merge", "side"]).status.success());
    let err = refused(alt(root, &["commit", "--amend", "-m", "x"]));
    assert!(err.contains("a merge is in progress"), "{err}");
    write(root, "a.txt", "both\n");
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "resolved"]));

    // once flow is on, main only moves through flow
    ok(alt(root, &["flow", "init"]));
    ok(alt(root, &["switch", "main"]));
    assert!(
        !alt(root, &["commit", "--amend", "-m", "x"])
            .status
            .success()
    );
}
