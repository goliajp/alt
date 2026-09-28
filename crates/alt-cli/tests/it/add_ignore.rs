//! `alt add .` honours `.gitignore`. The end-to-end smoke test the
//! self-hosting workflow blew up on: with no ignore handling, `alt add .`
//! used to swallow every gitignored path (caches, dev sandboxes, the
//! `.alt` store itself). This locks the fix in place.

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

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn alt_add_dot_skips_gitignored_paths_at_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));

    // The exact shape `alt`'s own project root uses
    std::fs::write(root.join(".gitignore"), "/.dev/\n/target/\n*.log\n").unwrap();

    // tracked
    std::fs::write(root.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "// hi\n").unwrap();

    // ignored — these used to be staged by an unfiltered `alt add .`
    std::fs::create_dir(root.join(".dev")).unwrap();
    std::fs::write(root.join(".dev/heavy.bin"), [0xff; 65536]).unwrap();
    std::fs::create_dir(root.join("target")).unwrap();
    std::fs::write(root.join("target/build.out"), [0xab; 4096]).unwrap();
    std::fs::write(root.join("run.log"), "noise\n").unwrap();

    let stdout = ok(alt(root, &["add", "."]));

    // staged count covers the tracked entries only:
    //   .gitignore, Cargo.toml, src/lib.rs
    let staged: usize = stdout
        .split_whitespace()
        .find_map(|tok| tok.parse::<usize>().ok())
        .unwrap_or_else(|| panic!("could not parse staged count from {stdout:?}"));
    assert_eq!(staged, 3, "want 3 tracked entries staged, got {stdout}");

    // and a status / commit cycle proves the ignored content doesn't reach
    // the store — a fresh commit would otherwise refuse to start (huge
    // staged delete) or carry the ignored payload into the tree.
    ok(alt(root, &["commit", "-m", "first"]));
    let log = ok(alt(root, &["log", "-n", "1", "--json"]));
    assert!(log.contains("\"tree\":"), "no tree in commit: {log}");
}

/// `alt` with the global-excludes environment pinned: `set` pairs are
/// exported, `unset` names are removed, so the developer's own excludes
/// file never leaks in.
fn alt_env(repo: &Path, args: &[&str], set: &[(&str, &Path)], unset: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_alt"));
    cmd.current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args);
    for (k, v) in set {
        cmd.env(k, v);
    }
    for k in unset {
        cmd.env_remove(k);
    }
    cmd.output().unwrap()
}

fn staged_after_add_dot(root: &Path, set: &[(&str, &Path)], unset: &[&str]) -> String {
    ok(alt_env(root, &["init", "."], set, unset));
    ok(alt_env(root, &["add", "."], set, unset));
    ok(alt_env(root, &["status"], set, unset))
}

fn scratch_tree(root: &Path) {
    std::fs::write(root.join("keep.txt"), "k\n").unwrap();
    std::fs::create_dir(root.join(".scratch")).unwrap();
    std::fs::write(root.join(".scratch/notes.md"), "n\n").unwrap();
    std::fs::write(root.join("trace.out"), "t\n").unwrap();
}

#[test]
fn the_global_excludes_file_under_xdg_config_home_applies() {
    let repo = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    std::fs::create_dir(config.path().join("git")).unwrap();
    std::fs::write(config.path().join("git/ignore"), ".scratch/\n*.out\n").unwrap();
    scratch_tree(repo.path());

    let st = staged_after_add_dot(repo.path(), &[("XDG_CONFIG_HOME", config.path())], &[]);
    assert!(st.contains("keep.txt"), "{st}");
    assert!(!st.contains(".scratch"), "{st}");
    assert!(!st.contains("trace.out"), "{st}");
}

#[test]
fn without_xdg_config_home_the_file_under_home_applies() {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".config/git")).unwrap();
    std::fs::write(home.path().join(".config/git/ignore"), "*.out\n").unwrap();
    scratch_tree(repo.path());

    let st = staged_after_add_dot(repo.path(), &[("HOME", home.path())], &["XDG_CONFIG_HOME"]);
    assert!(!st.contains("trace.out"), "{st}");
    assert!(st.contains(".scratch/notes.md"), "{st}");
}

#[test]
fn a_repository_gitignore_overrides_the_global_excludes() {
    let repo = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    std::fs::create_dir(config.path().join("git")).unwrap();
    std::fs::write(config.path().join("git/ignore"), "*.out\n").unwrap();
    scratch_tree(repo.path());
    std::fs::write(repo.path().join(".gitignore"), "!trace.out\n").unwrap();

    let st = staged_after_add_dot(repo.path(), &[("XDG_CONFIG_HOME", config.path())], &[]);
    assert!(st.contains("trace.out"), "{st}");
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

/// Tracks `logs/keep.log`, then starts ignoring `*.log` and `logs/`; drops
/// an untracked log beside it and edits `a.txt`.
fn tracked_then_ignored(root: &Path, run: &dyn Fn(&[&str]) -> Output) {
    std::fs::create_dir(root.join("logs")).unwrap();
    std::fs::write(root.join("logs/keep.log"), "keep\n").unwrap();
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    ok(run(&["add", "."]));
    ok(run(&["commit", "-m", "base"]));
    std::fs::write(root.join(".gitignore"), "*.log\nlogs/\n").unwrap();
    std::fs::write(root.join("logs/new.log"), "untracked\n").unwrap();
    std::fs::write(root.join("a.txt"), "a2\n").unwrap();
    ok(run(&["add", "."]));
    ok(run(&["commit", "-m", "ignore logs"]));
}

#[test]
fn ignore_rules_do_not_hide_tracked_files() {
    let (a, g) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, g) = (a.path(), g.path());
    ok(alt(a, &["init", "."]));
    ok(git(g, &["init", "-q", "-b", "main"]));
    tracked_then_ignored(a, &|args| alt(a, args));
    tracked_then_ignored(g, &|args| git(g, args));

    // `add .` kept the tracked log and left the untracked one out, as git did
    let tree = |cat: String| cat.lines().next().unwrap().to_owned();
    assert_eq!(
        tree(ok(alt(a, &["cat-file", "-p", "main"]))),
        tree(ok(git(g, &["cat-file", "-p", "HEAD"])))
    );
    assert!(ok(alt(a, &["status"])).contains("working tree clean"));

    // and an edit to the tracked-but-ignored file still shows
    std::fs::write(a.join("logs/keep.log"), "changed\n").unwrap();
    let st = ok(alt(a, &["status"]));
    assert!(st.contains("modified:   logs/keep.log"), "{st}");
    assert!(!st.contains("new.log"), "{st}");
}
