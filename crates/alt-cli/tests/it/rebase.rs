//! `alt rebase` and `alt rebase -i`, each checked against git running the
//! same steps with the same editor scripts.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn alt_env(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_alt"));
    c.current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_EDITOR", "true")
        .args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn git_env(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new("git");
    c.arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
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

struct Pair {
    _dirs: (tempfile::TempDir, tempfile::TempDir),
    a: PathBuf,
    g: PathBuf,
}

impl Pair {
    fn new() -> Pair {
        let dirs = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, g) = (dirs.0.path().to_owned(), dirs.1.path().to_owned());
        ok(alt_env(&a, &["init", "."], &[]));
        ok(git_env(&g, &["init", "-q", "-b", "main"], &[]));
        Pair { _dirs: dirs, a, g }
    }

    /// Runs `args` in both with `env`; asserts they agree on success.
    fn both_env(&self, args: &[&str], env: &[(&str, &str)]) -> bool {
        let (x, y) = (alt_env(&self.a, args, env), git_env(&self.g, args, env));
        assert_eq!(
            x.status.success(),
            y.status.success(),
            "alt and git disagree on {args:?}\nalt: {}{}\ngit: {}{}",
            String::from_utf8_lossy(&x.stdout),
            String::from_utf8_lossy(&x.stderr),
            String::from_utf8_lossy(&y.stdout),
            String::from_utf8_lossy(&y.stderr)
        );
        x.status.success()
    }

    fn both(&self, args: &[&str]) -> bool {
        self.both_env(args, &[])
    }

    fn write(&self, file: &str, body: &str) {
        for root in [&self.a, &self.g] {
            std::fs::write(root.join(file), body).unwrap();
        }
    }

    fn commit(&self, file: &str, body: &str, msg: &str) {
        self.write(file, body);
        assert!(self.both(&["add", "."]));
        assert!(self.both(&["commit", "-m", msg]));
    }

    /// (tree line, parent count, message) of `rev` in each, asserted equal.
    fn same(&self, rev: &str) -> (String, usize, String) {
        let x = shape(&ok(alt_env(&self.a, &["cat-file", "-p", rev], &[])));
        let y = shape(&ok(git_env(&self.g, &["cat-file", "-p", rev], &[])));
        assert_eq!(x, y, "{rev} differs between alt and git");
        x
    }

    fn tip(&self) -> String {
        ok(alt_env(&self.a, &["rev-parse", "HEAD"], &[]))
    }
}

fn shape(cat: &str) -> (String, usize, String) {
    let (head, msg) = cat.split_once("\n\n").unwrap();
    (
        head.lines().next().unwrap().to_owned(),
        head.lines().filter(|l| l.starts_with("parent ")).count(),
        msg.to_owned(),
    )
}

/// An editor script, shared by both tools, written next to the repos.
fn script(dir: &Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut perm = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&path, perm).unwrap();
    path.to_str().unwrap().to_owned()
}

/// Sets the command on todo line `n` (1-based) to `action`.
fn set_action(dir: &Path, n: usize, action: &str) -> String {
    script(
        dir,
        &format!("todo-{n}-{action}"),
        &format!(
            "awk -v n={n} -v a={action} 'NR==n {{$1=a}} {{print}}' \"$1\" > \"$1.tmp\" && mv \"$1.tmp\" \"$1\""
        ),
    )
}

/// main: base, then m1. topic (from base): t1 on x.txt, t2 on y.txt.
fn diverged(p: &Pair) {
    p.commit("f.txt", "base\n", "base");
    assert!(p.both(&["branch", "topic"]));
    p.commit("m.txt", "m\n", "m1");
    assert!(p.both(&["switch", "topic"]));
    p.commit("x.txt", "x\n", "t1");
    p.commit("y.txt", "y\n", "t2");
}

#[test]
fn rebasing_a_branch_onto_another_matches_git() {
    let p = Pair::new();
    diverged(&p);
    assert!(p.both(&["rebase", "main"]));
    assert_eq!(p.same("HEAD").2, "t2\n");
    assert_eq!(p.same("HEAD~1").2, "t1\n");
    assert_eq!(p.same("HEAD~2").2, "m1\n");
    // the replayed commits keep their author
    let head = ok(alt_env(&p.a, &["cat-file", "-p", "topic"], &[]));
    assert!(head.contains("\nauthor tester <t@e> "), "{head}");
    // and a second rebase has nothing to do
    let again = alt_env(&p.a, &["rebase", "main"], &[]);
    assert!(String::from_utf8_lossy(&again.stdout).contains("is up to date"));
}

#[test]
fn squash_fixup_reword_and_drop_match_git() {
    let dir = tempfile::tempdir().unwrap();
    for (line, action, expect) in [
        (2, "squash", vec!["t1\n\nt2\n", "m1\n"]),
        (2, "fixup", vec!["t1\n", "m1\n"]),
        (2, "drop", vec!["t1\n", "m1\n"]),
        (1, "reword", vec!["t2\n", "reworded\n", "m1\n"]),
    ] {
        let p = Pair::new();
        diverged(&p);
        let seq = set_action(dir.path(), line, action);
        let msg = script(dir.path(), "reword-msg", "printf 'reworded\\n' > \"$1\"");
        let env = [
            ("GIT_SEQUENCE_EDITOR", seq.as_str()),
            ("GIT_EDITOR", msg.as_str()),
        ];
        let env = if action == "reword" {
            &env[..]
        } else {
            &env[..1]
        };
        assert!(p.both_env(&["rebase", "-i", "main"], env), "{action}");
        for (i, want) in expect.iter().enumerate() {
            assert_eq!(&p.same(&format!("HEAD~{i}")).2, want, "{action} HEAD~{i}");
        }
    }
}

#[test]
fn a_conflict_stops_and_continue_finishes_as_in_git() {
    let p = Pair::new();
    p.commit("f.txt", "1\n2\n3\n", "base");
    assert!(p.both(&["branch", "topic"]));
    p.commit("f.txt", "1\nmain\n3\n", "on main");
    assert!(p.both(&["switch", "topic"]));
    p.commit("f.txt", "1\ntopic\n3\n", "on topic");
    p.commit("g.txt", "g\n", "then g");

    assert!(!p.both(&["rebase", "main"]));
    // mid-rebase the branch has not moved and other commands wait
    assert!(!alt_env(&p.a, &["switch", "main"], &[]).status.success());
    p.write("f.txt", "1\nboth\n3\n");
    assert!(p.both(&["add", "."]));
    assert!(p.both(&["rebase", "--continue"]));
    assert_eq!(p.same("HEAD").2, "then g\n");
    assert_eq!(p.same("HEAD~1").2, "on topic\n");
    assert_eq!(
        std::fs::read_to_string(p.a.join("f.txt")).unwrap(),
        "1\nboth\n3\n"
    );
    ok(alt_env(&p.a, &["switch", "main"], &[]));
}

#[test]
fn skip_drops_the_stopped_commit_as_in_git() {
    let p = Pair::new();
    p.commit("f.txt", "1\n2\n3\n", "base");
    assert!(p.both(&["branch", "topic"]));
    p.commit("f.txt", "1\nmain\n3\n", "on main");
    assert!(p.both(&["switch", "topic"]));
    p.commit("f.txt", "1\ntopic\n3\n", "on topic");
    p.commit("g.txt", "g\n", "then g");
    assert!(!p.both(&["rebase", "main"]));
    assert!(p.both(&["rebase", "--skip"]));
    assert_eq!(p.same("HEAD").2, "then g\n");
    assert_eq!(p.same("HEAD~1").2, "on main\n");
}

#[test]
fn abort_and_undo_both_take_the_rebase_back() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let alt = |args: &[&str]| alt_env(root, args, &[]);
    ok(alt(&["init", "."]));
    std::fs::write(root.join("f.txt"), "1\n2\n3\n").unwrap();
    ok(alt(&["add", "."]));
    ok(alt(&["commit", "-m", "base"]));
    ok(alt(&["branch", "topic"]));
    std::fs::write(root.join("f.txt"), "1\nmain\n3\n").unwrap();
    ok(alt(&["add", "."]));
    ok(alt(&["commit", "-m", "on main"]));
    ok(alt(&["switch", "topic"]));
    std::fs::write(root.join("g.txt"), "g\n").unwrap();
    ok(alt(&["add", "."]));
    ok(alt(&["commit", "-m", "add g"]));
    std::fs::write(root.join("f.txt"), "1\ntopic\n3\n").unwrap();
    ok(alt(&["add", "."]));
    ok(alt(&["commit", "-m", "on topic"]));
    let before = ok(alt(&["rev-parse", "topic"]));

    // the first commit replays, the second conflicts: abort rewinds it all
    assert!(!alt(&["rebase", "main"]).status.success());
    ok(alt(&["rebase", "--abort"]));
    assert_eq!(ok(alt(&["rev-parse", "topic"])), before);
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "1\ntopic\n3\n"
    );
    assert!(ok(alt(&["status"])).contains("On branch topic"));
    assert!(ok(alt(&["status"])).contains("working tree clean"));

    // a finished rebase is one op: undo returns the branch and files
    let p = Pair::new();
    diverged(&p);
    let tip = p.tip();
    ok(alt_env(&p.a, &["rebase", "main"], &[]));
    assert_ne!(p.tip(), tip);
    ok(alt_env(&p.a, &["undo"], &[]));
    assert_eq!(p.tip(), tip);
    assert!(!p.a.join("m.txt").exists());
}

#[test]
fn a_commit_already_upstream_is_dropped_as_in_git() {
    let p = Pair::new();
    p.commit("f.txt", "base\n", "base");
    assert!(p.both(&["branch", "topic"]));
    p.commit("x.txt", "x\n", "same change");
    assert!(p.both(&["switch", "topic"]));
    p.commit("x.txt", "x\n", "same change, made again");
    p.commit("y.txt", "y\n", "own change");
    assert!(p.both(&["rebase", "main"]));
    assert_eq!(p.same("HEAD").2, "own change\n");
    assert_eq!(p.same("HEAD~1").2, "same change\n");
}
