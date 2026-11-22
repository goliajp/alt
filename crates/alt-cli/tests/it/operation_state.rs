//! `alt status --json` names the operation a workspace is in the middle
//! of, and how far a sequence of commits has come.

use std::path::Path;
use std::process::Output;

fn run(repo: &Path, args: &[&str]) -> Output {
    crate::alt_command()
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn alt(repo: &Path, args: &[&str]) -> String {
    let done = run(repo, args);
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert!(done.status.success(), "{args:?}: {stderr}");
    String::from_utf8(done.stdout).unwrap()
}

/// Commits `text` as the whole of `f.txt`.
fn commit(repo: &Path, text: &str, message: &str) {
    std::fs::write(repo.join("f.txt"), text).unwrap();
    alt(repo, &["add", "f.txt"]);
    alt(repo, &["commit", "-m", message]);
}

/// The `operation` member of `status --json`, as it is written.
fn operation(repo: &Path) -> String {
    let status = alt(repo, &["status", "--json"]);
    let from = status.find("\"operation\":").expect("an operation member") + 12;
    let to = status[from..].find(",\"clean\"").unwrap();
    status[from..from + to].to_owned()
}

/// `main` and `side` each with their own change to the same line.
fn diverged() -> alt_os::temp::TempDir {
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    alt(repo, &["init", "."]);
    commit(repo, "base\n", "base");
    alt(repo, &["switch", "-c", "feature/side"]);
    commit(repo, "side\n", "side");
    alt(repo, &["switch", "main"]);
    commit(repo, "main\n", "main");
    dir
}

#[test]
fn nothing_is_under_way_in_a_settled_workspace() {
    let dir = diverged();
    assert_eq!(operation(dir.path()), "null");
}

#[test]
fn a_merge_that_stopped_is_under_way_until_it_is_committed() {
    let dir = diverged();
    let repo = dir.path();
    assert_eq!(run(repo, &["merge", "feature/side"]).status.code(), Some(1));
    assert_eq!(operation(repo), r#"{"kind":"merge"}"#);
    std::fs::write(repo.join("f.txt"), "both\n").unwrap();
    alt(repo, &["add", "f.txt"]);
    assert_eq!(operation(repo), r#"{"kind":"merge"}"#);
    alt(repo, &["commit", "-m", "merged"]);
    assert_eq!(operation(repo), "null");
}

#[test]
fn a_rebase_says_which_of_its_commits_it_stands_at() {
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    alt(repo, &["init", "."]);
    commit(repo, "base\n", "base");
    alt(repo, &["switch", "-c", "feature/three"]);
    std::fs::write(repo.join("other.txt"), "one\n").unwrap();
    alt(repo, &["add", "other.txt"]);
    alt(repo, &["commit", "-m", "first of three"]);
    commit(repo, "ours\n", "second of three");
    std::fs::write(repo.join("other.txt"), "three\n").unwrap();
    alt(repo, &["add", "other.txt"]);
    alt(repo, &["commit", "-m", "third of three"]);
    alt(repo, &["switch", "main"]);
    commit(repo, "theirs\n", "main");
    alt(repo, &["switch", "feature/three"]);
    assert_eq!(run(repo, &["rebase", "main"]).status.code(), Some(1));
    assert_eq!(operation(repo), r#"{"kind":"rebase","step":2,"steps":3}"#);
    std::fs::write(repo.join("f.txt"), "settled\n").unwrap();
    alt(repo, &["add", "f.txt"]);
    alt(repo, &["rebase", "--continue"]);
    assert_eq!(operation(repo), "null");
}

#[test]
fn a_revert_of_several_commits_counts_them_and_one_alone_does_not() {
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    alt(repo, &["init", "."]);
    commit(repo, "one\n", "one");
    std::fs::write(repo.join("other.txt"), "x\n").unwrap();
    alt(repo, &["add", "other.txt"]);
    alt(repo, &["commit", "-m", "adds other"]);
    commit(repo, "two\n", "two");
    commit(repo, "three\n", "three");
    // undoing `two` under `three` conflicts; `adds other` goes first
    let stopped = run(repo, &["revert", "HEAD~2", "HEAD~1"]);
    assert_eq!(stopped.status.code(), Some(1));
    assert_eq!(operation(repo), r#"{"kind":"revert","step":2,"steps":2}"#);
    alt(repo, &["revert", "--abort"]);
    assert_eq!(operation(repo), "null");
    assert_eq!(run(repo, &["revert", "HEAD~1"]).status.code(), Some(1));
    assert_eq!(operation(repo), r#"{"kind":"revert"}"#);
}

#[test]
fn a_cherry_pick_that_stopped_is_under_way_until_it_is_committed() {
    let dir = diverged();
    let repo = dir.path();
    let marker = repo.join(".alt/CHERRY_PICK_HEAD");
    let tip = alt(repo, &["rev-parse", "HEAD"]);
    let picked = alt(repo, &["rev-parse", "feature/side"]);
    let stopped = run(repo, &["cherry-pick", "feature/side"]);
    assert_eq!(stopped.status.code(), Some(1));
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        format!("picked {}head {}", picked, tip)
    );
    assert_eq!(operation(repo), r#"{"kind":"cherry-pick"}"#);
    std::fs::write(repo.join("f.txt"), "both\n").unwrap();
    alt(repo, &["add", "f.txt"]);
    assert_eq!(operation(repo), r#"{"kind":"cherry-pick"}"#);
    alt(repo, &["commit", "-m", "picked"]);
    assert_eq!(operation(repo), "null");
    assert!(!marker.exists());
}

#[test]
fn a_cherry_pick_head_has_moved_away_from_is_over_and_the_next_write_forgets_it() {
    let dir = diverged();
    let repo = dir.path();
    let marker = repo.join(".alt/CHERRY_PICK_HEAD");
    let stopped = run(repo, &["cherry-pick", "feature/side"]);
    assert_eq!(stopped.status.code(), Some(1));
    let left = std::fs::read(&marker).unwrap();
    std::fs::write(repo.join("f.txt"), "both\n").unwrap();
    alt(repo, &["add", "f.txt"]);
    alt(repo, &["commit", "-m", "picked"]);
    // an older alt commits without removing the file: HEAD has moved on
    std::fs::write(&marker, left).unwrap();
    assert_eq!(operation(repo), "null");
    // a command that only reads leaves the file
    assert!(marker.exists());
    alt(repo, &["branch", "after"]);
    assert!(!marker.exists());
    assert_eq!(operation(repo), "null");
}

#[test]
fn a_cherry_pick_that_goes_through_leaves_nothing() {
    let dir = diverged();
    let repo = dir.path();
    std::fs::write(repo.join("other.txt"), "o\n").unwrap();
    alt(repo, &["add", "other.txt"]);
    alt(repo, &["commit", "-m", "other"]);
    alt(repo, &["switch", "feature/side"]);
    alt(repo, &["cherry-pick", "main"]);
    assert!(!repo.join(".alt/CHERRY_PICK_HEAD").exists());
    assert_eq!(operation(repo), "null");
}

#[test]
fn a_bisect_is_under_way_until_it_is_reset() {
    let dir = diverged();
    let repo = dir.path();
    alt(repo, &["bisect", "start"]);
    assert_eq!(operation(repo), r#"{"kind":"bisect"}"#);
    alt(repo, &["bisect", "reset"]);
    assert_eq!(operation(repo), "null");
}

#[test]
fn other_commands_are_refused_by_the_name_of_what_is_under_way() {
    let dir = diverged();
    let repo = dir.path();
    alt(repo, &["bisect", "start"]);
    let refused = run(repo, &["rebase", "feature/side"]);
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(
        said.contains("rebase: a bisect is in progress; end it with `alt bisect reset`"),
        "{said}"
    );
    alt(repo, &["bisect", "reset"]);
    assert_eq!(run(repo, &["merge", "feature/side"]).status.code(), Some(1));
    let refused = run(repo, &["rebase", "feature/side"]);
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(
        said.contains("rebase: a merge is in progress; resolve the conflicts and commit it"),
        "{said}"
    );
}
