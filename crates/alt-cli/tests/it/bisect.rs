//! `alt bisect`: finds the commit that introduced a change, stepping by hand
//! or with `run`, and agrees with git on the answer.

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

/// Twelve commits; the seventh adds `bug.txt`, which stays from then on.
fn history(root: &Path, run: &dyn Fn(&[&str]) -> Output) {
    for i in 1..=12 {
        std::fs::write(root.join("n.txt"), format!("{i}\n")).unwrap();
        if i == 7 {
            std::fs::write(root.join("bug.txt"), "bug\n").unwrap();
        }
        ok(run(&["add", "."]));
        ok(run(&["commit", "-m", &format!("c{i}")]));
    }
}

/// The subject named in "<oid> is the first bad commit" output.
fn first_bad(out: &str, subject_of: &dyn Fn(&str) -> String) -> String {
    let oid = out
        .lines()
        // git's `bisect run` quotes the term: "is the first 'bad' commit"
        .find_map(|l| {
            l.strip_suffix(" is the first bad commit")
                .or_else(|| l.strip_suffix(" is the first 'bad' commit"))
        })
        .unwrap_or_else(|| panic!("no verdict in:\n{out}"));
    subject_of(oid)
}

fn subject(cat: &str) -> String {
    cat.split_once("\n\n")
        .unwrap()
        .1
        .lines()
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn bisect_run_finds_the_same_commit_as_git() {
    let (a, g) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, g) = (a.path(), g.path());
    ok(alt(a, &["init", "."]));
    ok(git(g, &["init", "-q", "-b", "main"]));
    history(a, &|args| alt(a, args));
    history(g, &|args| git(g, args));

    let judge = ["sh", "-c", "test ! -f bug.txt"];
    ok(alt(a, &["bisect", "start", "main", "main~11"]));
    let alt_out = ok(alt(a, &[&["bisect", "run"][..], &judge].concat()));
    ok(git(g, &["bisect", "start", "main", "main~11"]));
    let git_out = ok(git(g, &[&["bisect", "run"][..], &judge].concat()));

    let alt_bad = first_bad(&alt_out, &|o| subject(&ok(alt(a, &["cat-file", "-p", o]))));
    let git_bad = first_bad(&git_out, &|o| subject(&ok(git(g, &["cat-file", "-p", o]))));
    assert_eq!(alt_bad, git_bad);
    assert_eq!(alt_bad, "c7");
    ok(alt(a, &["bisect", "reset"]));
}

#[test]
fn stepping_by_hand_narrows_to_the_first_bad_commit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    history(root, &|args| alt(root, args));
    let mut out = ok(alt(root, &["bisect", "start"]));
    assert!(out.contains("waiting for good and bad commits"), "{out}");
    out = ok(alt(root, &["bisect", "bad"]));
    assert!(
        out.contains("waiting for good commit(s), bad commit known"),
        "{out}"
    );
    out = ok(alt(root, &["bisect", "good", "main~11"]));
    let mut steps = 0;
    while !out.contains("is the first bad commit") {
        assert!(out.contains("Bisecting: "), "{out}");
        let verdict = if root.join("bug.txt").exists() {
            "bad"
        } else {
            "good"
        };
        out = ok(alt(root, &["bisect", verdict]));
        steps += 1;
        assert!(steps < 6, "12 commits need at most 4 steps");
    }
    let bad = first_bad(&out, &|o| subject(&ok(alt(root, &["cat-file", "-p", o]))));
    assert_eq!(bad, "c7");
}

#[test]
fn reset_returns_to_the_branch_and_other_commands_wait_meanwhile() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    history(root, &|args| alt(root, args));
    let tip = ok(alt(root, &["rev-parse", "main"]));
    ok(alt(root, &["bisect", "start", "main", "main~11"]));
    assert_ne!(std::fs::read_to_string(root.join("n.txt")).unwrap(), "12\n");

    assert!(!alt(root, &["switch", "-c", "x"]).status.success());
    let c = alt(root, &["commit", "--allow-empty", "-m", "mid-bisect"]);
    assert!(String::from_utf8_lossy(&c.stderr).contains("a bisect is in progress"));

    ok(alt(root, &["bisect", "reset"]));
    assert_eq!(ok(alt(root, &["rev-parse", "main"])), tip);
    assert_eq!(std::fs::read_to_string(root.join("n.txt")).unwrap(), "12\n");
    let st = ok(alt(root, &["status"]));
    assert!(
        st.contains("On branch main") && st.contains("working tree clean"),
        "{st}"
    );
}

#[test]
fn skipped_commits_are_stepped_around() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    history(root, &|args| alt(root, args));
    // c3 cannot be tested; the search goes around it and still ends on c7
    // (skipping c6 instead would leave c6 or c7 undecided, as in git)
    ok(alt(root, &["bisect", "start", "main", "main~11"]));
    ok(alt(root, &["bisect", "skip", "main~9"]));
    let out = ok(alt(
        root,
        &["bisect", "run", "sh", "-c", "test ! -f bug.txt"],
    ));
    let bad = first_bad(&out, &|o| subject(&ok(alt(root, &["cat-file", "-p", o]))));
    assert_eq!(bad, "c7");
    ok(alt(root, &["bisect", "reset"]));
    assert_eq!(
        ok(alt(root, &["rev-parse", "HEAD"])),
        ok(alt(root, &["rev-parse", "main"]))
    );
}
