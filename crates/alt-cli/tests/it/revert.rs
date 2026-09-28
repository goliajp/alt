//! `alt revert`: commits undoing earlier ones, checked against git running
//! the same steps. Oids inside messages differ between the two (commit times
//! differ), so messages are compared with oids masked.

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
        .env("GIT_EDITOR", "true")
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

/// A pair of repositories driven through the same steps.
struct Pair {
    _dirs: (tempfile::TempDir, tempfile::TempDir),
    a: std::path::PathBuf,
    g: std::path::PathBuf,
}

impl Pair {
    fn new() -> Pair {
        let dirs = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, g) = (dirs.0.path().to_owned(), dirs.1.path().to_owned());
        ok(alt(&a, &["init", "."]));
        ok(git(&g, &["init", "-q", "-b", "main"]));
        Pair { _dirs: dirs, a, g }
    }

    /// Runs `args` in both; asserts they agree on success.
    fn both(&self, args: &[&str]) -> bool {
        let (x, y) = (alt(&self.a, args), git(&self.g, args));
        assert_eq!(
            x.status.success(),
            y.status.success(),
            "alt and git disagree on {args:?}\nalt: {}\ngit: {}",
            String::from_utf8_lossy(&x.stderr),
            String::from_utf8_lossy(&y.stderr)
        );
        x.status.success()
    }

    fn commit(&self, file: &str, body: &str, msg: &str) {
        for root in [&self.a, &self.g] {
            std::fs::write(root.join(file), body).unwrap();
        }
        assert!(self.both(&["add", "."]));
        assert!(self.both(&["commit", "-m", msg]));
    }

    /// The commit `rev` in each, as (tree line, parent count, masked message).
    fn shapes(&self, rev: &str) -> ((String, usize, String), (String, usize, String)) {
        (
            shape(&ok(alt(&self.a, &["cat-file", "-p", rev]))),
            shape(&ok(git(&self.g, &["cat-file", "-p", rev]))),
        )
    }

    fn assert_same(&self, rev: &str) -> String {
        let (x, y) = self.shapes(rev);
        assert_eq!(x, y, "{rev} differs");
        x.2
    }

    fn write(&self, file: &str, body: &str) {
        for root in [&self.a, &self.g] {
            std::fs::write(root.join(file), body).unwrap();
        }
    }
}

fn shape(cat: &str) -> (String, usize, String) {
    let (head, msg) = cat.split_once("\n\n").unwrap();
    let masked: String = msg
        .split_inclusive(|c: char| !c.is_ascii_hexdigit())
        .map(|w| {
            let hex = w.trim_end_matches(|c: char| !c.is_ascii_hexdigit());
            if hex.len() == 40 {
                w.replacen(hex, "<oid>", 1)
            } else {
                w.to_owned()
            }
        })
        .collect();
    (
        head.lines().next().unwrap().to_owned(),
        head.lines().filter(|l| l.starts_with("parent ")).count(),
        masked,
    )
}

fn history(p: &Pair) {
    p.commit("a.txt", "1\n2\n3\n", "first");
    p.commit("b.txt", "b\n", "add b");
    p.commit("a.txt", "1\n2\nthree\n", "edit a");
}

#[test]
fn reverting_a_commit_matches_git() {
    let p = Pair::new();
    history(&p);
    assert!(p.both(&["revert", "--no-edit", "HEAD~1"]));
    let msg = p.assert_same("HEAD");
    assert_eq!(msg, "Revert \"add b\"\n\nThis reverts commit <oid>.\n");
    assert!(!p.a.join("b.txt").exists());

    // and reverting the revert brings it back, titled as git titles it
    assert!(p.both(&["revert", "--no-edit", "HEAD"]));
    let msg = p.assert_same("HEAD");
    assert!(msg.starts_with("Reapply \"add b\""), "{msg}");
    assert!(p.a.join("b.txt").exists());
}

#[test]
fn several_reverts_apply_in_order_as_in_git() {
    let p = Pair::new();
    history(&p);
    assert!(p.both(&["revert", "--no-edit", "HEAD", "HEAD~1"]));
    assert!(p.assert_same("HEAD").starts_with("Revert \"add b\""));
    assert!(p.assert_same("HEAD~1").starts_with("Revert \"edit a\""));
    assert_eq!(
        std::fs::read_to_string(p.a.join("a.txt")).unwrap(),
        "1\n2\n3\n"
    );
}

#[test]
fn a_conflicting_revert_stops_and_continues_as_in_git() {
    let p = Pair::new();
    p.commit("f.txt", "1\n2\n3\n", "base");
    p.commit("f.txt", "1\nb\n3\n", "to b");
    p.commit("f.txt", "1\nc\n3\n", "to c");
    assert!(!p.both(&["revert", "--no-edit", "HEAD~1"]));
    // the branch has not moved; switching away is refused until it is done
    assert!(!alt(&p.a, &["switch", "-c", "elsewhere"]).status.success());

    p.write("f.txt", "1\nresolved\n3\n");
    assert!(p.both(&["add", "."]));
    assert!(p.both(&["revert", "--continue"]));
    let msg = p.assert_same("HEAD");
    assert!(msg.starts_with("Revert \"to b\""), "{msg}");
    assert_eq!(
        std::fs::read_to_string(p.a.join("f.txt")).unwrap(),
        "1\nresolved\n3\n"
    );
    assert!(!alt(&p.a, &["revert", "--continue"]).status.success());
}

#[test]
fn abort_puts_everything_back() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    for (body, msg) in [
        ("1\n2\n3\n", "base"),
        ("1\nb\n3\n", "to b"),
        ("1\nc\n3\n", "to c"),
    ] {
        std::fs::write(root.join("f.txt"), body).unwrap();
        ok(alt(root, &["add", "."]));
        ok(alt(root, &["commit", "-m", msg]));
    }
    std::fs::write(root.join("g.txt"), "g\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "add g"]));
    let before = ok(alt(root, &["rev-parse", "main"]));

    // the first revert commits, the second conflicts: abort undoes both
    assert!(!alt(root, &["revert", "HEAD", "HEAD~2"]).status.success());
    assert!(!root.join("g.txt").exists());
    ok(alt(root, &["revert", "--abort"]));
    assert_eq!(ok(alt(root, &["rev-parse", "main"])), before);
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "1\nc\n3\n"
    );
    assert!(root.join("g.txt").exists());
    assert!(ok(alt(root, &["status"])).contains("working tree clean"));
}

#[test]
fn a_commit_mid_sequence_concludes_one_revert_and_continue_does_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    for (file, body, msg) in [
        ("f.txt", "1\n2\n3\n", "base"),
        ("f.txt", "1\nb\n3\n", "to b"),
        ("f.txt", "1\nc\n3\n", "to c"),
        ("g.txt", "g\n", "add g"),
    ] {
        std::fs::write(root.join(file), body).unwrap();
        ok(alt(root, &["add", "."]));
        ok(alt(root, &["commit", "-m", msg]));
    }
    assert!(!alt(root, &["revert", "HEAD~2", "HEAD"]).status.success());
    std::fs::write(root.join("f.txt"), "1\nmine\n3\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "my own revert of to b"]));
    ok(alt(root, &["revert", "--continue"]));
    let head = ok(alt(root, &["cat-file", "-p", "main"]));
    assert!(head.contains("Revert \"add g\""), "{head}");
    assert!(!root.join("g.txt").exists());
}

#[test]
fn reverting_a_merge_needs_a_mainline_as_in_git() {
    let p = Pair::new();
    p.commit("a.txt", "a\n", "base");
    assert!(p.both(&["branch", "side"]));
    p.commit("m.txt", "m\n", "on main");
    assert!(p.both(&["switch", "side"]));
    p.commit("s.txt", "s\n", "on side");
    assert!(p.both(&["switch", "main"]));
    // git needs --no-edit to skip the editor; alt never opens one
    ok(alt(&p.a, &["merge", "side"]));
    ok(git(&p.g, &["merge", "-q", "--no-edit", "side"]));

    assert!(!p.both(&["revert", "--no-edit", "HEAD"]));
    assert!(p.both(&["revert", "--no-edit", "-m", "1", "HEAD"]));
    let msg = p.assert_same("HEAD");
    assert!(msg.contains("reversing\nchanges made to <oid>."), "{msg}");
    assert!(!p.a.join("s.txt").exists());
    assert!(p.a.join("m.txt").exists());
}
