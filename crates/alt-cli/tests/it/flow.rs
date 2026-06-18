//! The git-flow workflow engine: `alt flow init`, `feature start/finish`, and
//! `alt undo`. A finish is one atomic op (merging the feature into develop and
//! deleting it); undo inverts it. Results export to a git-clean repo.

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

/// init + one commit on main, then `flow init`. Returns the repo root dir.
fn seed(root: &Path) {
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("app.txt"), "v1\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "initial"]));
    ok(alt(root, &["flow", "init"]));
}

#[test]
fn feature_cycle_fast_forward_then_undo() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    assert!(ok(alt(root, &["branch"])).contains("* develop"));

    // start a feature, work on it, finish it (develop hasn't moved => ff)
    ok(alt(root, &["flow", "feature", "start", "login"]));
    assert!(ok(alt(root, &["branch"])).contains("* feature/login"));
    std::fs::write(root.join("app.txt"), "v1\nlogin\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "add login"]));

    let fin = ok(alt(root, &["flow", "feature", "finish", "login"]));
    assert!(
        fin.contains("Merged 'feature/login' into 'develop'"),
        "{fin}"
    );
    let branches = ok(alt(root, &["branch"]));
    assert!(
        branches.contains("* develop"),
        "back on develop: {branches}"
    );
    assert!(
        !branches.contains("feature/login"),
        "feature deleted: {branches}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("app.txt")).unwrap(),
        "v1\nlogin\n"
    );

    // exported repo is git-clean and develop carries the feature work
    let out = tempfile::tempdir().unwrap();
    let target = out.path().join("exported");
    ok(alt(root, &["export", target.to_str().unwrap()]));
    assert!(git(&target, &["fsck", "--strict"]).status.success());

    // undo the finish: the feature branch returns and HEAD moves back to it
    let u = ok(alt(root, &["undo"]));
    assert!(u.contains("Undid"), "{u}");
    let after = ok(alt(root, &["branch"]));
    assert!(
        after.contains("* feature/login"),
        "feature restored: {after}"
    );
}

#[test]
fn finish_with_diverged_develop_makes_a_merge_commit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);

    // feature changes one file
    ok(alt(root, &["flow", "feature", "start", "a"]));
    std::fs::write(root.join("fa.txt"), "feature\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "feature file"]));

    // develop independently changes another file -> histories diverge.
    // The protected-branch guard refuses commits on `develop` by
    // design (alt-only-flow stance); we override here because this
    // test deliberately models the "diverged develop" scenario alt
    // must still merge cleanly when it inherits one from a git mirror
    // or a recovery operation.
    ok(alt(root, &["switch", "develop"]));
    std::fs::write(root.join("fd.txt"), "develop\n").unwrap();
    ok(alt(root, &["add", "."]));
    let out = Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(root)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("ALT_PROTECTED_OVERRIDE", "1")
        .args(["commit", "-m", "develop file"])
        .output()
        .unwrap();
    ok(out);

    // finishing now needs a real three-way merge commit
    let fin = ok(alt(root, &["flow", "feature", "finish", "a"]));
    assert!(fin.contains("Merged 'feature/a' into 'develop'"), "{fin}");
    // both files present on develop
    assert!(root.join("fa.txt").exists() && root.join("fd.txt").exists());

    // export and confirm the develop tip is a two-parent merge over git-clean
    let out = tempfile::tempdir().unwrap();
    let target = out.path().join("exported");
    ok(alt(root, &["export", target.to_str().unwrap()]));
    assert!(git(&target, &["fsck", "--strict"]).status.success());
    let parents = ok(git(
        &target,
        &["rev-list", "--parents", "-n", "1", "develop"],
    ));
    assert_eq!(
        parents.split_whitespace().count(),
        3,
        "merge commit has two parents: {parents}"
    );
    let ls = ok(git(&target, &["ls-tree", "-r", "--name-only", "develop"]));
    assert!(ls.contains("fa.txt") && ls.contains("fd.txt"), "{ls}");
}
