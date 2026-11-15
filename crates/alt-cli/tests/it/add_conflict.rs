//! `alt add` on a conflicted path whose file was deleted resolves the
//! conflict as a removal, however the path is reached: by name, through
//! its directory, or with the whole tree. As in git.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
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

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// A merge stopped by a conflict in `d/c.txt`, whose file is then deleted.
fn conflict_with_the_file_gone(root: &Path, run: &dyn Fn(&[&str]) -> Output) {
    write(root, "d/c.txt", "c1\n");
    write(root, "d/keep.txt", "keep\n");
    ok(run(&["add", "."]));
    ok(run(&["commit", "-m", "first"]));
    ok(run(&["switch", "-c", "side"]));
    write(root, "d/c.txt", "c2\n");
    ok(run(&["add", "."]));
    ok(run(&["commit", "-m", "second"]));
    ok(run(&["switch", "main"]));
    write(root, "d/c.txt", "c3\n");
    ok(run(&["add", "."]));
    ok(run(&["commit", "-m", "third"]));
    assert!(!run(&["merge", "side"]).status.success());
    std::fs::remove_file(root.join("d/c.txt")).unwrap();
}

fn tree_of(cat: &str) -> String {
    cat.lines().next().unwrap().to_owned()
}

#[test]
fn a_conflicted_path_without_its_file_is_resolved_as_removed_like_git() {
    for spec in ["d/c.txt", "d", "./d/", "."] {
        let (a, g) = (
            alt_os::temp::tempdir().unwrap(),
            alt_os::temp::tempdir().unwrap(),
        );
        let (a, g) = (a.path(), g.path());
        ok(alt(a, &["init", "."]));
        ok(git(g, &["init", "-q", "-b", "main"]));
        conflict_with_the_file_gone(a, &|args| alt(a, args));
        conflict_with_the_file_gone(g, &|args| git(g, args));
        let before = ok(alt(a, &["status"]));
        assert!(
            before.contains("Unmerged paths:") && before.contains("d/c.txt"),
            "{spec}: {before}"
        );

        ok(alt(a, &["add", spec]));
        ok(git(g, &["add", spec]));
        assert_eq!(
            ok(git(g, &["status", "--porcelain"])),
            "D  d/c.txt\n",
            "{spec}"
        );
        assert_eq!(
            ok(alt(a, &["status"])),
            "On branch main\nChanges to be committed:\n\tdeleted:   d/c.txt\n",
            "{spec}"
        );
        ok(alt(a, &["commit", "-m", "merged"]));
        ok(git(g, &["commit", "-q", "-m", "merged"]));
        assert_eq!(
            tree_of(&ok(alt(a, &["cat-file", "-p", "main"]))),
            tree_of(&ok(git(g, &["cat-file", "-p", "HEAD"]))),
            "{spec}"
        );
    }
}

#[test]
fn a_path_that_is_neither_tracked_nor_there_still_matches_nothing() {
    let (a, g) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (a, g) = (a.path(), g.path());
    ok(alt(a, &["init", "."]));
    ok(git(g, &["init", "-q", "-b", "main"]));
    conflict_with_the_file_gone(a, &|args| alt(a, args));
    conflict_with_the_file_gone(g, &|args| git(g, args));
    let before = ok(alt(a, &["status"]));
    for run in [
        &alt(a, &["add", "d/missing"]),
        &git(g, &["add", "d/missing"]),
    ] {
        assert!(!run.status.success());
        let said = String::from_utf8_lossy(&run.stderr);
        assert!(
            said.contains("pathspec 'd/missing' did not match any files"),
            "{said}"
        );
    }
    assert_eq!(ok(alt(a, &["status"])), before);
    assert_eq!(ok(git(g, &["status", "--porcelain"])), "UU d/c.txt\n");
}
