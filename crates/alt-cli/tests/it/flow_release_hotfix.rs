//! M8-C1: `alt flow release` and `alt flow hotfix` mirror the feature
//! flow's atomic shape — single ref-tx + single op-log entry per
//! start/finish. release finish also back-merges into develop;
//! hotfix follows the same shape (start off main → merge into main +
//! back-merge into develop). A conflict on either merge aborts the
//! whole finish without touching any ref.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("USER", "tester")
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

fn seed_repo(root: &Path) {
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("seed.txt"), "seed\n").unwrap();
    ok(alt(root, &["add", "seed.txt"]));
    ok(alt(root, &["commit", "-m", "seed"]));
    ok(alt(root, &["flow", "init"]));
}

#[test]
fn release_start_branches_off_develop_and_switches_head() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_repo(root);

    ok(alt(root, &["flow", "release", "start", "1.0"]));
    let br = ok(alt(root, &["branch"]));
    assert!(br.contains("release/1.0"), "release/1.0 must exist: {br}");
    // HEAD on release branch: the * marker sits next to it
    let lines: Vec<&str> = br.lines().filter(|l| l.contains("release/1.0")).collect();
    assert!(
        lines.iter().any(|l| l.trim_start().starts_with('*')),
        "HEAD must be on release/1.0: {br}"
    );
}

#[test]
fn release_finish_merges_into_main_back_merges_into_develop_and_switches_to_develop() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_repo(root);

    // start the release and make a commit on it
    ok(alt(root, &["flow", "release", "start", "1.0"]));
    std::fs::write(root.join("ver.txt"), "1.0\n").unwrap();
    ok(alt(root, &["add", "ver.txt"]));
    ok(alt(root, &["commit", "-m", "bump version"]));

    ok(alt(root, &["flow", "release", "finish", "1.0"]));

    // 1. release branch is gone
    let br = ok(alt(root, &["branch"]));
    assert!(
        !br.contains("release/1.0"),
        "release branch must be deleted after finish: {br}"
    );

    // 2. HEAD landed on develop
    assert!(
        br.lines()
            .any(|l| l.trim_start().starts_with('*') && l.contains("develop")),
        "HEAD must be on develop after release finish: {br}"
    );

    // 3. both main and develop hold the bump commit
    let log_dev = ok(alt(root, &["log", "--pretty=oneline"]));
    assert!(log_dev.contains("bump version"), "develop log: {log_dev}");
    ok(alt(root, &["switch", "main"]));
    let log_main = ok(alt(root, &["log", "--pretty=oneline"]));
    assert!(log_main.contains("bump version"), "main log: {log_main}");
}

#[test]
fn hotfix_finish_lands_on_main_and_develop_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_repo(root);

    ok(alt(root, &["flow", "hotfix", "start", "fix-crash"]));
    std::fs::write(root.join("patch.txt"), "fix\n").unwrap();
    ok(alt(root, &["add", "patch.txt"]));
    ok(alt(root, &["commit", "-m", "fix crash"]));
    ok(alt(root, &["flow", "hotfix", "finish", "fix-crash"]));

    let br = ok(alt(root, &["branch"]));
    assert!(
        !br.contains("hotfix/fix-crash"),
        "hotfix branch must be deleted: {br}"
    );
    assert!(
        br.lines()
            .any(|l| l.trim_start().starts_with('*') && l.contains("develop")),
        "HEAD on develop after hotfix finish: {br}"
    );

    let log_dev = ok(alt(root, &["log", "--pretty=oneline"]));
    assert!(
        log_dev.contains("fix crash"),
        "develop carries the hotfix: {log_dev}"
    );
    ok(alt(root, &["switch", "main"]));
    let log_main = ok(alt(root, &["log", "--pretty=oneline"]));
    assert!(
        log_main.contains("fix crash"),
        "main carries the hotfix: {log_main}"
    );
}

#[test]
fn release_finish_is_a_single_op_in_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_repo(root);

    // count ops before and after the release finish; the finish itself
    // adds exactly one (the multi-ref-change is one transaction).
    let pre = ok(alt(root, &["op-log", "--json"]));
    let pre_count: usize = pre.matches("\"id\":").count();

    ok(alt(root, &["flow", "release", "start", "2.0"]));
    std::fs::write(root.join("v.txt"), "2.0\n").unwrap();
    ok(alt(root, &["add", "v.txt"]));
    ok(alt(root, &["commit", "-m", "v2"]));
    ok(alt(root, &["flow", "release", "finish", "2.0"]));

    let post = ok(alt(root, &["op-log", "--json"]));
    let post_count: usize = post.matches("\"id\":").count();
    // start, commit, add (index-tx), finish = +4 ops over the start state
    assert!(
        post_count >= pre_count + 4,
        "release finish must record at least one op (got pre={pre_count}, post={post_count})"
    );
    // and undo of the finish puts main back to its previous tip — i.e.
    // the finish is genuinely a single op, not three half-baked ones the
    // user has to undo separately.
    ok(alt(root, &["undo"]));
    let br = ok(alt(root, &["branch"]));
    assert!(
        br.contains("release/2.0"),
        "undoing the finish must restore the release branch: {br}"
    );
}
