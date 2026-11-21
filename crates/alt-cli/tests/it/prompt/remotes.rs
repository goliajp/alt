//! How far the branch is from the branch of its name on each remote.

use super::{alt, commit, committed, head, set_ref, state, told};

#[test]
fn a_remote_without_a_branch_of_the_name_is_not_mentioned() {
    let dir = committed();
    let repo = dir.path();
    alt(
        repo,
        &["remote", "add", "origin", "https://example.invalid/r.git"],
    );
    set_ref(repo, "refs/remotes/origin/other", &head(repo));
    assert_eq!(state(repo), "workspace default\nbranch main\n");
}

#[test]
fn commits_ahead_and_behind_are_counted_for_every_remote() {
    let dir = committed();
    let repo = dir.path();
    alt(
        repo,
        &["remote", "add", "origin", "https://example.invalid/r.git"],
    );
    alt(
        repo,
        &["remote", "add", "backup", "https://example.invalid/b.git"],
    );
    alt(repo, &["branch", "theirs"]);
    set_ref(repo, "refs/remotes/origin/main", &head(repo));
    assert_eq!(
        state(repo),
        "workspace default\nbranch main\nremote origin 0 0\n"
    );
    commit(repo, "one\n", "one");
    commit(repo, "two\n", "two");
    assert_eq!(
        state(repo),
        "workspace default\nbranch main\nremote origin 2 0\n"
    );
    // theirs went on from the base by three commits of its own
    alt(repo, &["switch", "theirs"]);
    for text in ["a\n", "b\n", "c\n"] {
        commit(repo, text, "theirs");
    }
    set_ref(repo, "refs/remotes/backup/main", &head(repo));
    alt(repo, &["switch", "main"]);
    assert_eq!(
        state(repo),
        "workspace default\nbranch main\nremote backup 2 3\nremote origin 2 0\n"
    );
    assert_eq!(
        told(repo, &["--no-ahead-behind"], false),
        "workspace default\nbranch main\n"
    );
    let zsh = String::from_utf8(super::run(repo, &["prompt", "--format=zsh"]).stdout).unwrap();
    assert!(
        zsh.ends_with("'remote:backup' '2 3' 'remote:origin' '2 0' remotes 'backup origin')\n"),
        "{zsh}"
    );
}

#[test]
fn a_branch_behind_its_remote_says_by_how_much() {
    let dir = committed();
    let repo = dir.path();
    alt(
        repo,
        &["remote", "add", "origin", "https://example.invalid/r.git"],
    );
    alt(repo, &["branch", "kept"]);
    commit(repo, "one\n", "one");
    set_ref(repo, "refs/remotes/origin/kept", &head(repo));
    alt(repo, &["switch", "kept"]);
    assert_eq!(
        state(repo),
        "workspace default\nbranch kept\nremote origin 0 1\n"
    );
}
