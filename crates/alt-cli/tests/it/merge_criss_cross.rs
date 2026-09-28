//! Merges whose histories cross: the two sides have more than one best common
//! ancestor. Each scenario is built in alt and in git with the same steps, and
//! alt must reach git's verdict.

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

/// One tool's view of the steps both scenarios are written in.
trait Vcs {
    fn run(&self, args: &[&str]) -> Output;
    fn root(&self) -> &Path;
    fn new_branch(&self, name: &str);
    fn switch(&self, name: &str);

    fn commit_file(&self, content: &str, msg: &str) {
        std::fs::write(self.root().join("f.txt"), content).unwrap();
        ok(self.run(&["add", "."]));
        ok(self.run(&["commit", "-m", msg]));
    }

    /// Merges `branch`, which must conflict, then records `resolution`.
    fn merge_resolving(&self, branch: &str, resolution: &str) {
        assert!(!self.run(&["merge", branch]).status.success());
        self.commit_file(resolution, "resolve");
    }
}

struct Alt<'a>(&'a Path);
struct Git<'a>(&'a Path);

impl Vcs for Alt<'_> {
    fn run(&self, args: &[&str]) -> Output {
        alt(self.0, args)
    }
    fn root(&self) -> &Path {
        self.0
    }
    fn new_branch(&self, name: &str) {
        ok(alt(self.0, &["branch", name]));
    }
    fn switch(&self, name: &str) {
        ok(alt(self.0, &["switch", name]));
    }
}

impl Vcs for Git<'_> {
    fn run(&self, args: &[&str]) -> Output {
        let mut full = vec!["-c", "merge.conflictStyle=merge"];
        if args[0] == "merge" {
            full.extend(["merge", "--no-edit"]);
            full.extend(&args[1..]);
        } else if args[0] == "commit" {
            full.extend(["commit", "-q"]);
            full.extend(&args[1..]);
        } else {
            full.extend(args);
        }
        git(self.0, &full)
    }
    fn root(&self) -> &Path {
        self.0
    }
    fn new_branch(&self, name: &str) {
        ok(git(self.0, &["branch", name]));
    }
    fn switch(&self, name: &str) {
        ok(git(self.0, &["switch", "-q", name]));
    }
}

/// x and y change the same line differently, then each merges the other and
/// keeps its own version. The histories now cross: x1 and y1 are both best
/// common ancestors of x2 and y2.
fn crossed_disagreement(v: &dyn Vcs) {
    v.commit_file("1\n2\n3\n", "base");
    v.new_branch("x");
    v.new_branch("y");
    v.switch("x");
    v.commit_file("1\na\n3\n", "x1");
    v.new_branch("x1");
    v.switch("y");
    v.commit_file("1\nb\n3\n", "y1");
    v.new_branch("y1");
    // each side merges the other's original tip, so neither merge is a
    // fast-forward of the other
    v.switch("x");
    v.merge_resolving("y1", "1\na\n3\n");
    v.switch("y");
    v.merge_resolving("x1", "1\nb\n3\n");
    v.switch("x");
}

#[test]
fn a_crossed_disagreement_conflicts_as_in_git() {
    let (a, g) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    ok(alt(a.path(), &["init", "."]));
    ok(git(g.path(), &["init", "-q", "-b", "main"]));
    crossed_disagreement(&Alt(a.path()));
    crossed_disagreement(&Git(g.path()));

    // picking either base alone would silently keep one side's line
    assert!(!Git(g.path()).run(&["merge", "y"]).status.success());
    let m = alt(a.path(), &["merge", "y"]);
    assert!(
        !m.status.success(),
        "alt merged cleanly: {}",
        std::fs::read_to_string(a.path().join("f.txt")).unwrap()
    );
    let f = std::fs::read_to_string(a.path().join("f.txt")).unwrap();
    assert!(
        f.contains("<<<<<<< HEAD\na\n=======\nb\n>>>>>>> y\n"),
        "{f}"
    );
}

#[test]
fn a_clean_criss_cross_merge_matches_git() {
    let (a, g) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    ok(alt(a.path(), &["init", "."]));
    ok(git(g.path(), &["init", "-q", "-b", "main"]));
    for v in [&Alt(a.path()) as &dyn Vcs, &Git(g.path())] {
        // x and y touch different lines, then cross-merge cleanly
        v.commit_file("1\n2\n3\n4\n5\n", "base");
        v.new_branch("x");
        v.new_branch("y");
        v.switch("x");
        v.commit_file("X\n2\n3\n4\n5\n", "x1");
        v.new_branch("x1");
        v.switch("y");
        v.commit_file("1\n2\n3\n4\nY\n", "y1");
        v.new_branch("y1");
        v.switch("x");
        ok(v.run(&["merge", "y1"]));
        v.switch("y");
        ok(v.run(&["merge", "x1"]));
        // then each side moves on before merging again
        v.commit_file("X\n2\n3\n4\nY2\n", "y3");
        v.switch("x");
        v.commit_file("X\n2\nZ\n4\nY\n", "x3");
        ok(v.run(&["merge", "y"]));
    }
    let alt_f = std::fs::read_to_string(a.path().join("f.txt")).unwrap();
    let git_f = std::fs::read_to_string(g.path().join("f.txt")).unwrap();
    assert_eq!(alt_f, git_f);
    assert_eq!(alt_f, "X\n2\nZ\n4\nY2\n");
}

#[test]
fn a_resolved_conflict_commits_as_a_merge() {
    let a = tempfile::tempdir().unwrap();
    let root = a.path();
    ok(alt(root, &["init", "."]));
    let v = Alt(root);
    v.commit_file("1\n", "base");
    v.new_branch("y");
    v.commit_file("main\n", "main");
    v.switch("y");
    v.commit_file("y\n", "y");
    v.switch("main");
    assert!(!alt(root, &["merge", "y"]).status.success());

    // the merge is still open: switching away would strand it
    let err = alt(root, &["switch", "y"]);
    assert!(!err.status.success());
    assert!(String::from_utf8_lossy(&err.stderr).contains("a merge is in progress"));

    v.commit_file("both\n", "resolve");
    let head = ok(alt(root, &["cat-file", "-p", "main"]));
    let parents = head.lines().filter(|l| l.starts_with("parent ")).count();
    assert_eq!(parents, 2, "{head}");
    // the merge is done: a new merge of y has nothing to bring in
    assert!(ok(alt(root, &["merge", "y"])).contains("Already up to date"));
    ok(alt(root, &["switch", "y"]));
}
