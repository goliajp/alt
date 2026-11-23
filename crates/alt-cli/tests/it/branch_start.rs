//! A branch made at a commit that is named: `alt branch <name> <start>`,
//! `alt switch -c <name> <start>` and `alt checkout -b <name> <start>`,
//! each beside git's.

use std::path::{Path, PathBuf};

use crate::like_git::{alt, alt_daemon, fails, files, git, heads, index, ok};

/// A clone of a git repository and an import of that clone.
struct Cloned {
    _dir: alt_os::temp::TempDir,
    alt: PathBuf,
    git: PathBuf,
}

/// `main` with two commits and `topic` with a third at the remote, the
/// tag `v1` at the first and the annotated tag `v2` at the second; in
/// the clone, the branch `side` at the first commit, and `main` checked
/// out.
fn cloned() -> Cloned {
    let dir = alt_os::temp::tempdir().unwrap();
    let at = |name: &str| dir.path().join(name);
    let (up, clone, imported) = (at("up"), at("clone"), at("imported"));
    std::fs::create_dir_all(&up).unwrap();
    ok(git(&up, &["init", "-q", "."]));
    let commit = |files: &[(&str, &str)], message: &str| {
        for (name, content) in files {
            std::fs::write(up.join(name), content).unwrap();
        }
        ok(git(&up, &["add", "."]));
        ok(git(&up, &["commit", "-q", "-m", message]));
    };
    commit(&[("a", "1\n"), ("b", "1\n")], "first");
    ok(git(&up, &["tag", "v1"]));
    commit(&[("a", "2\n")], "second");
    ok(git(&up, &["tag", "-a", "-m", "annotated", "v2"]));
    ok(git(&up, &["switch", "-q", "-c", "topic"]));
    commit(&[("t", "t\n")], "third");
    ok(git(&up, &["switch", "-q", "main"]));
    let (source, to) = (up.to_str().unwrap(), clone.to_str().unwrap());
    ok(git(dir.path(), &["clone", "-q", source, to]));
    ok(git(&clone, &["branch", "side", "HEAD~1"]));
    ok(alt(&clone, &["import", imported.to_str().unwrap()]));
    Cloned {
        _dir: dir,
        alt: imported,
        git: clone,
    }
}

impl Cloned {
    fn write(&self, path: &str, content: &str) {
        for root in [&self.alt, &self.git] {
            std::fs::write(root.join(path), content).unwrap();
        }
    }

    fn assert_same_state(&self) {
        assert_eq!(index(&self.alt), ok(git(&self.git, &["ls-files", "-s"])));
        assert_eq!(files(&self.alt), files(&self.git));
        assert_eq!(heads(&self.alt), heads(&self.git));
    }

    /// The command fails in both with `code`, alt saying what git says.
    fn same_failure(&self, args: &[&str], code: i32) -> String {
        let said = fails(alt(&self.alt, args), code);
        let git_said = fails(git(&self.git, args), code);
        assert!(said.starts_with(&git_said), "alt: {said}\ngit: {git_said}");
        git_said
    }

    /// What each branch named follows, in both the same.
    fn same_upstream(&self, branch: &str) -> String {
        let name = format!("{branch}@{{u}}");
        let args = ["rev-parse", "--abbrev-ref", name.as_str()];
        let followed = ok(alt(&self.alt, &args));
        assert_eq!(followed, ok(git(&self.git, &args)), "{branch}");
        followed
    }
}

const TRACKS: &str = "set up to track 'origin/topic'.\n";

fn short(repo: &Path, rev: &str) -> String {
    ok(git(repo, &["rev-parse", "--short", rev]))
        .trim()
        .to_owned()
}

#[test]
fn a_branch_is_made_where_a_branch_a_tag_or_a_commit_is_named() {
    let both = cloned();
    let abbreviated = short(&both.git, "HEAD~1");
    for (name, start) in [
        ("n1", "side"),
        ("n2", "v1"),
        ("n3", "v2"),
        ("n4", "HEAD~1"),
        ("n5", abbreviated.as_str()),
        ("n6", "refs/heads/side"),
        ("n7", "origin/main~1"),
    ] {
        let said = ok(alt(&both.alt, &["branch", name, start]));
        assert_eq!(ok(git(&both.git, &["branch", name, start])), "", "{start}");
        assert!(!said.contains("track"), "{start}: {said}");
        both.assert_same_state();
    }
    // one made at a branch of a remote follows it
    for (name, start) in [("t1", "origin/topic"), ("t2", "refs/remotes/origin/topic")] {
        let said = ok(alt(&both.alt, &["branch", name, start]));
        let git_said = ok(git(&both.git, &["branch", name, start]));
        assert_eq!(git_said, format!("branch '{name}' {TRACKS}"));
        assert!(said.ends_with(&git_said), "{said}");
        both.assert_same_state();
        assert_eq!(both.same_upstream(name), "origin/topic\n");
    }
    // and no longer once it is deleted and made anew elsewhere
    ok(alt(&both.alt, &["branch", "-d", "t1"]));
    ok(alt(&both.alt, &["branch", "t1", "side"]));
    fails(alt(&both.alt, &["rev-parse", "t1@{u}"]), 128);
}

#[test]
fn a_start_that_names_no_commit_and_a_name_taken_are_refused() {
    let both = cloned();
    let at = heads(&both.alt);
    assert_eq!(
        both.same_failure(&["branch", "n", "nosuch"], 128),
        "fatal: not a valid object name: 'nosuch'\n"
    );
    let said = both.same_failure(&["branch", "n", "HEAD:a"], 128);
    assert!(
        said.ends_with("fatal: not a valid branch point: 'HEAD:a'\n"),
        "{said}"
    );
    assert_eq!(
        both.same_failure(&["branch", "side", "main"], 128),
        "fatal: a branch named 'side' already exists\n"
    );
    assert_eq!(
        both.same_failure(&["switch", "-c", "n", "nosuch"], 128),
        "fatal: invalid reference: nosuch\n"
    );
    assert_eq!(
        both.same_failure(&["switch", "-c", "side", "main"], 128),
        "fatal: a branch named 'side' already exists\n"
    );
    assert_eq!(
        both.same_failure(&["switch", "side", "main"], 128),
        "fatal: only one reference expected\n"
    );
    assert_eq!(
        both.same_failure(&["checkout", "-b", "n", "nosuch"], 128),
        "fatal: 'nosuch' is not a commit and a branch 'n' cannot be created from it\n"
    );
    assert_eq!(
        both.same_failure(&["checkout", "-b", "n", "--", "a"], 128),
        "fatal: 'a' is not a commit and a branch 'n' cannot be created from it\n"
    );
    assert_eq!(
        both.same_failure(&["checkout", "-b", "n", "main", "a"], 128),
        "fatal: Cannot update paths and switch to branch 'n' at the same time.\n"
    );
    assert_eq!(
        both.same_failure(&["checkout", "-b", "side", "main"], 128),
        "fatal: a branch named 'side' already exists\n"
    );
    assert_eq!(heads(&both.alt), at);
    both.assert_same_state();
}

#[test]
fn a_switch_to_a_new_branch_checks_out_where_it_starts() {
    for verb in [&["switch", "-c"][..], &["checkout", "-b"]] {
        let both = cloned();
        both.write("b", "mine\n");
        let run = |name: &str, start: &str| {
            let args: Vec<&str> = verb.iter().copied().chain([name, start]).collect();
            (ok(alt(&both.alt, &args)), ok(git(&both.git, &args)))
        };
        // what is not committed goes along where the commits agree
        let (said, git_said) = run("s1", "side");
        assert_eq!(git_said, "M\tb\n");
        assert_eq!(said, "M\tb\nSwitched to a new branch 's1'\n");
        both.assert_same_state();
        assert_eq!(std::fs::read_to_string(both.alt.join("a")).unwrap(), "1\n");
        let (said, git_said) = run("s2", "origin/topic");
        assert_eq!(git_said, format!("M\tb\nbranch 's2' {TRACKS}"));
        let tracks = format!("M\tb\nSwitched to a new branch 's2'\nbranch 's2' {TRACKS}");
        assert_eq!(said, tracks);
        both.assert_same_state();
        assert_eq!(both.same_upstream("s2"), "origin/topic\n");
        let (_, _) = run("s3", "v2");
        both.assert_same_state();
        // and is refused, nothing made, where it would be written over
        both.write("a", "mine\n");
        let args: Vec<&str> = verb.iter().copied().chain(["s4", "side"]).collect();
        let said = both.same_failure(&args, 1);
        assert!(
            said.contains("would be overwritten by checkout:\n\ta\n"),
            "{said}"
        );
        both.assert_same_state();
        let listed = ok(alt(&both.alt, &["branch"]));
        assert!(!listed.contains("s4"), "{listed}");
    }
}

#[test]
fn a_switch_to_a_new_branch_elsewhere_is_one_step_to_undo() {
    let both = cloned();
    both.write("b", "mine\n");
    let (before, staged, at) = (files(&both.alt), index(&both.alt), heads(&both.alt));
    ok(alt(&both.alt, &["switch", "-c", "s1", "side"]));
    assert_ne!(files(&both.alt), before);
    ok(alt(&both.alt, &["undo"]));
    assert_eq!(
        (files(&both.alt), index(&both.alt), heads(&both.alt)),
        (before, staged, at)
    );
}

#[test]
fn the_daemon_and_a_parallel_workspace_start_branches_alike() {
    let (direct, served) = (cloned(), cloned());
    for args in [
        &["branch", "n1", "v1"][..],
        &["switch", "-c", "s1", "side"],
        &["switch", "-c", "s2", "origin/topic"],
        &["checkout", "-b", "s3", "v2"],
        &["rev-parse", "--abbrev-ref", "s2@{u}"],
    ] {
        assert_eq!(
            ok(alt_daemon(&served.alt, args)),
            ok(alt(&direct.alt, args)),
            "{args:?}"
        );
        assert_eq!(files(&served.alt), files(&direct.alt));
        assert_eq!(index(&served.alt), index(&direct.alt));
        assert_eq!(heads(&served.alt), heads(&direct.alt));
    }
    let json = ok(alt(&direct.alt, &["switch", "--json", "-c", "s4", "side"]));
    assert!(json.contains("\"branch\":\"s4\"") && json.contains("\"result\":\"created\""));
    assert!(!json.contains("Switched"), "{json}");

    let trees = alt_os::temp::tempdir().unwrap();
    let tree = trees.path().join("ws");
    let at = tree.to_str().unwrap();
    ok(alt(&direct.alt, &["workspace", "add", "ws", at]));
    let (mine, staged) = (files(&direct.alt), index(&direct.alt));
    ok(alt(&tree, &["switch", "-c", "w1", "origin/topic"]));
    assert_eq!(std::fs::read_to_string(tree.join("t")).unwrap(), "t\n");
    assert_eq!((files(&direct.alt), index(&direct.alt)), (mine, staged));
    let listed = ok(alt(&direct.alt, &["rev-parse", "w1", "origin/topic"]));
    let mut lines = listed.lines();
    assert_eq!(lines.next(), lines.next());
}
