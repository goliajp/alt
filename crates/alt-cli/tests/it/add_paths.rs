//! `alt add <dir>` stages everything under the directory, deletions included,
//! and a commit that would change nothing is refused — both as in git.

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

fn tree_line(cat: &str) -> String {
    cat.lines().next().unwrap().to_owned()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// The same steps for either tool: `run` executes one of its commands.
fn scenario(root: &Path, run: &dyn Fn(&[&str]) -> Output) {
    write(root, "top.txt", "t\n");
    write(root, "src/a.rs", "a\n");
    write(root, "src/deep/b.rs", "b\n");
    write(root, "other/c.txt", "c\n");
    ok(run(&["add", "."]));
    ok(run(&["commit", "-m", "base"]));
    // change and delete under src/, touch other/ and a look-alike too
    write(root, "src/a.rs", "a2\n");
    std::fs::remove_file(root.join("src/deep/b.rs")).unwrap();
    write(root, "src/new.rs", "n\n");
    write(root, "other/c.txt", "c2\n");
    write(root, "srcish.txt", "not under src\n");
    ok(run(&["add", "./src/"]));
    ok(run(&["commit", "-m", "src only"]));
}

#[test]
fn adding_a_directory_matches_git() {
    let (a, g) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, g) = (a.path(), g.path());
    ok(alt(a, &["init", "."]));
    ok(git(g, &["init", "-q", "-b", "main"]));
    scenario(a, &|args| alt(a, args));
    scenario(g, &|args| git(g, args));
    let alt_head = ok(alt(a, &["cat-file", "-p", "main"]));
    let git_head = ok(git(g, &["cat-file", "-p", "HEAD"]));
    assert_eq!(tree_line(&alt_head), tree_line(&git_head));
    // other/ and srcish.txt were left unstaged
    let st = ok(alt(a, &["status"]));
    assert!(
        st.contains("other/c.txt") && st.contains("srcish.txt"),
        "{st}"
    );
}

#[test]
fn a_path_matching_nothing_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    write(root, "a.txt", "a\n");
    let o = alt(root, &["add", "nope"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("pathspec 'nope' did not match any files"));
    assert!(!ok(alt(root, &["op-log"])).contains("verb=add"));
}

#[test]
fn a_commit_that_changes_nothing_is_refused_unless_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    write(root, "a.txt", "a\n");
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));
    let before = ok(alt(root, &["rev-parse", "main"]));

    let o = alt(root, &["commit", "-m", "nothing"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("nothing to commit"));
    assert_eq!(ok(alt(root, &["rev-parse", "main"])), before);

    ok(alt(root, &["commit", "--allow-empty", "-m", "on purpose"]));
    assert_ne!(ok(alt(root, &["rev-parse", "main"])), before);
}
