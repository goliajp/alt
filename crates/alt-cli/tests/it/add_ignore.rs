//! `alt add .` honours `.gitignore`. The end-to-end smoke test the
//! self-hosting workflow blew up on: with no ignore handling, `alt add .`
//! used to swallow every gitignored path (caches, dev sandboxes, the
//! `.alt` store itself). This locks the fix in place.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    crate::alt_command()
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
    let dir = alt_os::temp::tempdir().unwrap();
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
    let mut cmd = crate::alt_command();
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
    let repo = alt_os::temp::tempdir().unwrap();
    let config = alt_os::temp::tempdir().unwrap();
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
    let repo = alt_os::temp::tempdir().unwrap();
    let home = alt_os::temp::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".config/git")).unwrap();
    std::fs::write(home.path().join(".config/git/ignore"), "*.out\n").unwrap();
    scratch_tree(repo.path());

    let st = staged_after_add_dot(repo.path(), &[("HOME", home.path())], &["XDG_CONFIG_HOME"]);
    assert!(!st.contains("trace.out"), "{st}");
    assert!(st.contains(".scratch/notes.md"), "{st}");
}

#[test]
fn a_repository_gitignore_overrides_the_global_excludes() {
    let repo = alt_os::temp::tempdir().unwrap();
    let config = alt_os::temp::tempdir().unwrap();
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
    let (a, g) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
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

/// A pattern without a slash is held against the name of a path, not
/// against the directories above it: those are decided on their own.
fn rules_by_name(root: &Path, run: &dyn Fn(&[&str]) -> Output) {
    std::fs::write(root.join(".gitignore"), "gen\n!gen/\n*.d\n!keep.d\n").unwrap();
    for dir in ["gen", "keep.d", "sub/gen", "sub/keep.d"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
        // the directory is not hidden, and no rule names this file
        std::fs::write(root.join(dir).join("out.txt"), "kept\n").unwrap();
        // hidden by its own name in a directory that is not
        std::fs::write(root.join(dir).join("x.d"), "hidden\n").unwrap();
    }
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    ok(run(&["add", "."]));
    ok(run(&["commit", "-m", "base"]));
}

#[test]
fn a_rule_without_a_slash_goes_by_the_name_of_a_path_as_for_git() {
    let (a, g) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (a, g) = (a.path(), g.path());
    ok(alt(a, &["init", "."]));
    ok(git(g, &["init", "-q", "-b", "main"]));
    rules_by_name(a, &|args| alt(a, args));
    rules_by_name(g, &|args| git(g, args));
    let tree = |cat: String| cat.lines().next().unwrap().to_owned();
    assert_eq!(
        tree(ok(alt(a, &["cat-file", "-p", "main"]))),
        tree(ok(git(g, &["cat-file", "-p", "HEAD"])))
    );
    let listed = ok(git(g, &["ls-files"]));
    assert!(listed.contains("gen/out.txt") && listed.contains("sub/keep.d/out.txt"));
    assert!(!listed.contains("x.d"), "{listed}");
    // and what is left untracked is hidden for both
    let st = ok(alt(a, &["status"]));
    assert!(st.contains("working tree clean"), "{st}");
    assert_eq!(ok(git(g, &["status", "--porcelain"])), "");
}

/// Rules that end in a tab, in spaces and in a space after a backslash,
/// beside files whose names end the same ways and ones that do not.
fn rules_that_end_in_blanks(root: &Path, run: &dyn Fn(&[&str]) -> Output) {
    let rules = "tab.txt\t\nspace.txt  \nkept\\ \nboth\\   \n";
    std::fs::write(root.join(".gitignore"), rules).unwrap();
    let names = ["tab.txt", "tab.txt\t", "space.txt", "kept", "kept ", "both"];
    for name in names.into_iter().chain(["both "]) {
        std::fs::write(root.join(name), "x\n").unwrap();
    }
    ok(run(&["add", "."]));
    ok(run(&["commit", "-m", "base"]));
}

#[test]
fn a_rule_ends_as_for_git_in_a_tab_in_spaces_and_in_an_escaped_space() {
    let (a, g) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (a, g) = (a.path(), g.path());
    ok(alt(a, &["init", "."]));
    ok(git(g, &["init", "-q", "-b", "main"]));
    rules_that_end_in_blanks(a, &|args| alt(a, args));
    rules_that_end_in_blanks(g, &|args| git(g, args));
    let tree = |cat: String| cat.lines().next().unwrap().to_owned();
    assert_eq!(
        tree(ok(alt(a, &["cat-file", "-p", "main"]))),
        tree(ok(git(g, &["cat-file", "-p", "HEAD"])))
    );
    // a tab is part of the rule, spaces are not, an escaped space is
    let listed = ok(git(g, &["ls-files", "-z"]));
    assert_eq!(listed, ".gitignore\0both\0kept\0tab.txt\0");
}
