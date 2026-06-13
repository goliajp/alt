//! `alt merge`: fast-forward, a clean three-way merge whose result matches
//! git's own merge, and a conflict that can be resolved and committed.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
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
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn fast_forward_merge() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    // feat advances; main stays put, so merging feat fast-forwards
    ok(alt(root, &["switch", "-c", "feat"]));
    std::fs::write(root.join("a.txt"), "two\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "advance"]));
    ok(alt(root, &["switch", "main"]));
    let out = ok(alt(root, &["merge", "feat"]));
    assert!(out.contains("Fast-forward"), "{out}");
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "two\n"
    );
}

#[test]
fn clean_three_way_merge_matches_git() {
    // run the identical scenario in both alt and git, then check the merged
    // working tree is the same and the alt merge commit has two parents.
    let scenario = |drive: &dyn Fn(&Path, &[&str]) -> Output, root: &Path, init: &[&str]| {
        drive(root, init);
        std::fs::write(root.join("a.txt"), "base\n").unwrap();
        std::fs::write(root.join("b.txt"), "base\n").unwrap();
        drive(root, &["add", "."]);
        drive(root, &["commit", "-m", "base"]);
    };

    // --- alt ---
    let adir = tempfile::tempdir().unwrap();
    let aroot = adir.path();
    scenario(&|r, a| alt(r, a), aroot, &["init", "."]);
    ok(alt(aroot, &["branch", "feat"]));
    std::fs::write(aroot.join("a.txt"), "ours\n").unwrap();
    ok(alt(aroot, &["add", "."]));
    ok(alt(aroot, &["commit", "-m", "ours"]));
    ok(alt(aroot, &["switch", "feat"]));
    std::fs::write(aroot.join("b.txt"), "theirs\n").unwrap();
    ok(alt(aroot, &["add", "."]));
    ok(alt(aroot, &["commit", "-m", "theirs"]));
    ok(alt(aroot, &["switch", "main"]));
    let mout = ok(alt(aroot, &["merge", "feat"]));
    assert!(mout.contains("Merge made"), "{mout}");

    // --- git, same moves ---
    let gdir = tempfile::tempdir().unwrap();
    let groot = gdir.path();
    scenario(&|r, a| git(r, a), groot, &["init", "-q", "-b", "main", "."]);
    ok(git(groot, &["branch", "feat"]));
    std::fs::write(groot.join("a.txt"), "ours\n").unwrap();
    ok(git(groot, &["add", "."]));
    ok(git(groot, &["commit", "-qm", "ours"]));
    ok(git(groot, &["switch", "-q", "feat"]));
    std::fs::write(groot.join("b.txt"), "theirs\n").unwrap();
    ok(git(groot, &["add", "."]));
    ok(git(groot, &["commit", "-qm", "theirs"]));
    ok(git(groot, &["switch", "-q", "main"]));
    ok(git(groot, &["merge", "--no-edit", "feat"]));

    // both working trees agree
    assert_eq!(
        std::fs::read_to_string(aroot.join("a.txt")).unwrap(),
        std::fs::read_to_string(groot.join("a.txt")).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(aroot.join("b.txt")).unwrap(),
        std::fs::read_to_string(groot.join("b.txt")).unwrap()
    );

    // export the alt repo and let git confirm the merge commit's tree equals
    // git's own merge tree (semantic L1 equivalence) and the two parents
    let out = tempfile::tempdir().unwrap();
    let target = out.path().join("exported");
    ok(alt(aroot, &["export", target.to_str().unwrap()]));
    assert!(git(&target, &["fsck", "--strict"]).status.success());
    let alt_tree = ok(git(&target, &["rev-parse", "HEAD^{tree}"]));
    let git_tree = ok(git(groot, &["rev-parse", "HEAD^{tree}"]));
    assert_eq!(alt_tree, git_tree, "merged tree must equal git's");
    let parents = ok(git(&target, &["rev-list", "--parents", "-n", "1", "HEAD"]));
    assert_eq!(
        parents.split_whitespace().count(),
        3,
        "merge commit has two parents: {parents}"
    );
}

#[test]
fn conflict_then_resolve_and_commit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f.txt"), "line1\nline2\nline3\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    ok(alt(root, &["branch", "feat"]));
    std::fs::write(root.join("f.txt"), "line1\nOURS\nline3\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "ours"]));
    ok(alt(root, &["switch", "feat"]));
    std::fs::write(root.join("f.txt"), "line1\nTHEIRS\nline3\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "theirs"]));
    ok(alt(root, &["switch", "main"]));

    // the merge conflicts: exit code is non-zero and markers land in the file
    let m = alt(root, &["merge", "feat"]);
    assert!(!m.status.success(), "conflicting merge must fail");
    assert!(String::from_utf8_lossy(&m.stdout).contains("CONFLICT"));
    let conflicted = std::fs::read_to_string(root.join("f.txt")).unwrap();
    assert!(conflicted.contains("<<<<<<< HEAD"), "{conflicted}");
    assert!(conflicted.contains(">>>>>>> feat"), "{conflicted}");
    assert!(conflicted.contains("=======\n"), "{conflicted}");

    // status surfaces the unmerged path
    assert!(
        ok(alt(root, &["status"])).contains("Unmerged paths:"),
        "status should list the unmerged file"
    );

    // resolve, stage, commit — the loop closes
    std::fs::write(root.join("f.txt"), "line1\nRESOLVED\nline3\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "resolved"]));
    assert!(ok(alt(root, &["status"])).contains("working tree clean"));

    // exported repo is git-clean
    let out = tempfile::tempdir().unwrap();
    let target = out.path().join("exported");
    ok(alt(root, &["export", target.to_str().unwrap()]));
    assert!(git(&target, &["fsck", "--strict"]).status.success());
}
