//! `alt prompt`: the state a shell prompt shows, from one command.

use std::path::Path;
use std::process::Output;

use alt_refs::{RefChange, RefStore, RefTarget};

use super::unconfirmed::{Lost, power_cut};

mod remotes;
mod shell;

fn run_with(repo: &Path, args: &[&str], daemon: bool) -> Output {
    let mut cmd = crate::alt_command();
    cmd.current_dir(repo)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("USER", "tester")
        .env("ALT_DAEMON_IDLE_MS", "1500")
        .args(args);
    if !daemon {
        cmd.env("ALT_NO_DAEMON", "1");
    }
    cmd.output().unwrap()
}

fn run(repo: &Path, args: &[&str]) -> Output {
    run_with(repo, args, false)
}

fn alt(repo: &Path, args: &[&str]) -> String {
    let done = run(repo, args);
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert!(done.status.success(), "{args:?}: {stderr}");
    String::from_utf8(done.stdout).unwrap()
}

/// What `alt prompt` with `options` says after the line that names the
/// repository. It succeeds and says nothing on stderr.
fn told(repo: &Path, options: &[&str], daemon: bool) -> String {
    let mut args = vec!["prompt"];
    args.extend(options);
    let done = run_with(repo, &args, daemon);
    assert_eq!(done.status.code(), Some(0), "{done:?}");
    assert_eq!(String::from_utf8_lossy(&done.stderr), "");
    let said = String::from_utf8(done.stdout).unwrap();
    let (place, rest) = said.split_once('\n').unwrap();
    let root = repo.canonicalize().unwrap();
    let named = Path::new(place.strip_prefix("repo ").unwrap());
    assert_eq!(named.canonicalize().unwrap(), root, "{said}");
    rest.to_owned()
}

fn state(repo: &Path) -> String {
    told(repo, &[], false)
}

/// Commits `text` as the whole of `f.txt`.
fn commit(repo: &Path, text: &str, message: &str) {
    std::fs::write(repo.join("f.txt"), text).unwrap();
    alt(repo, &["add", "f.txt"]);
    alt(repo, &["commit", "-m", message]);
}

fn head(repo: &Path) -> String {
    alt(repo, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn set_ref(repo: &Path, name: &str, oid: &str) {
    let oid = alt_git_codec::ObjectId::from_hex(oid.as_bytes()).unwrap();
    let change = RefChange {
        name: name.to_owned(),
        old: None,
        new: Some(RefTarget::Oid(oid)),
    };
    let mut refs = RefStore::open(repo.join(".alt")).unwrap();
    refs.commit("test", 1_700_000_000_000, &[change]).unwrap();
}

fn committed() -> alt_os::temp::TempDir {
    let dir = alt_os::temp::tempdir().unwrap();
    alt(dir.path(), &["init", "."]);
    commit(dir.path(), "base\n", "base");
    dir
}

/// `main` and `feature/side` each with their own change to the same line.
fn diverged() -> alt_os::temp::TempDir {
    let dir = committed();
    let repo = dir.path();
    alt(repo, &["switch", "-c", "feature/side"]);
    commit(repo, "side\n", "side");
    alt(repo, &["switch", "main"]);
    commit(repo, "main\n", "main");
    dir
}

#[test]
fn outside_a_repository_nothing_is_said_and_the_exit_code_is_one() {
    let dir = alt_os::temp::tempdir().unwrap();
    let done = run(dir.path(), &["prompt"]);
    assert_eq!(done.status.code(), Some(1));
    assert!(done.stdout.is_empty() && done.stderr.is_empty(), "{done:?}");
}

#[test]
fn a_repository_that_does_not_open_needs_attention() {
    let dir = alt_os::temp::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".alt")).unwrap();
    assert_eq!(state(dir.path()), "attention damaged\n");
}

#[test]
fn head_is_on_a_branch_born_or_not_or_on_none_at_a_tag() {
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path();
    alt(repo, &["init", "."]);
    assert_eq!(state(repo), "workspace default\nunborn main\n");
    commit(repo, "base\n", "base");
    assert_eq!(state(repo), "workspace default\nbranch main\n");
    // a tag is told only where there is no branch to tell
    alt(repo, &["tag", "v1"]);
    assert_eq!(state(repo), "workspace default\nbranch main\n");
    let tip = head(repo);
    alt(repo, &["bisect", "start"]);
    let detached = format!("workspace default\ndetached {}\n", &tip[..8]);
    assert_eq!(state(repo), format!("{detached}tag v1\noperation bisect\n"));
    alt(repo, &["bisect", "reset"]);
    // a tag object is followed to its commit
    alt(repo, &["tag", "release", "-a", "-m", "the first"]);
    alt(repo, &["bisect", "start"]);
    assert_eq!(
        state(repo),
        format!("{detached}tag release\noperation bisect\n")
    );
}

#[test]
fn changes_are_counted_by_kind() {
    let dir = committed();
    let repo = dir.path();
    std::fs::write(repo.join("staged.txt"), "s\n").unwrap();
    alt(repo, &["add", "staged.txt"]);
    std::fs::write(repo.join("f.txt"), "changed\n").unwrap();
    std::fs::write(repo.join("new-1"), "n\n").unwrap();
    std::fs::write(repo.join("new-2"), "n\n").unwrap();
    assert_eq!(
        state(repo),
        "workspace default\nbranch main\nstaged 1\nunstaged 1\nuntracked 2\n"
    );
    assert_eq!(
        told(repo, &["--no-untracked"], false),
        "workspace default\nbranch main\nstaged 1\nunstaged 1\n"
    );
    let zsh = run(repo, &["prompt", "--format=zsh"]);
    let zsh = String::from_utf8(zsh.stdout).unwrap();
    assert!(zsh.starts_with("typeset -gA alt_prompt=('repo' '"), "{zsh}");
    assert!(
        zsh.ends_with(
            "'workspace' 'default' 'branch' 'main' 'staged' '1' 'unstaged' '1' 'untracked' '2')\n"
        ),
        "{zsh}"
    );
}

#[test]
fn an_operation_under_way_is_named_with_its_step() {
    let dir = diverged();
    let repo = dir.path();
    assert_eq!(run(repo, &["merge", "feature/side"]).status.code(), Some(1));
    assert_eq!(
        state(repo),
        "workspace default\nbranch main\noperation merge\nunmerged 1\n"
    );

    let dir = diverged();
    let repo = dir.path();
    let stopped = run(repo, &["cherry-pick", "feature/side"]);
    assert_eq!(stopped.status.code(), Some(1));
    assert_eq!(
        state(repo),
        "workspace default\nbranch main\noperation cherry-pick\nunmerged 1\n"
    );

    let dir = diverged();
    let repo = dir.path();
    commit(repo, "main again\n", "again");
    assert_eq!(run(repo, &["revert", "HEAD~1"]).status.code(), Some(1));
    assert_eq!(
        state(repo),
        "workspace default\nbranch main\noperation revert\nunmerged 1\n"
    );
}

#[test]
fn a_rebase_keeps_the_name_of_its_branch_and_a_bisect_has_none() {
    let dir = diverged();
    let repo = dir.path();
    alt(repo, &["switch", "feature/side"]);
    std::fs::write(repo.join("other.txt"), "o\n").unwrap();
    alt(repo, &["add", "other.txt"]);
    alt(repo, &["commit", "-m", "second on side"]);
    assert_eq!(run(repo, &["rebase", "main"]).status.code(), Some(1));
    assert_eq!(
        state(repo),
        "workspace default\nbranch feature/side\noperation rebase\nstep 1 2\nunmerged 1\n"
    );
    alt(repo, &["rebase", "--abort"]);
    assert_eq!(state(repo), "workspace default\nbranch feature/side\n");

    alt(repo, &["bisect", "start"]);
    let tip = head(repo);
    assert_eq!(
        state(repo),
        format!(
            "workspace default\ndetached {}\noperation bisect\n",
            &tip[..8]
        )
    );
}

#[test]
fn a_parallel_workspace_says_its_own_name_and_branch() {
    let dir = committed();
    let repo = dir.path();
    alt(repo, &["branch", "topic"]);
    let other = alt_os::temp::tempdir().unwrap();
    let place = other.path().join("w");
    alt(
        repo,
        &["workspace", "add", "side", place.to_str().unwrap(), "topic"],
    );
    std::fs::write(place.join("new"), "n\n").unwrap();
    assert_eq!(state(&place), "workspace side\nbranch topic\nuntracked 1\n");
    assert_eq!(state(repo), "workspace default\nbranch main\n");
}

#[test]
fn operations_that_were_never_synced_need_attention() {
    let dir = alt_os::temp::tempdir().unwrap();
    power_cut(dir.path(), Lost::NotesCommit);
    let said = state(dir.path());
    assert!(said.ends_with("attention unconfirmed\n"), "{said}");
    assert!(
        said.starts_with("workspace default\nbranch main\n"),
        "{said}"
    );
}

#[test]
fn the_daemon_says_what_the_command_itself_says() {
    let dir = diverged();
    let repo = dir.path();
    std::fs::write(repo.join("new"), "n\n").unwrap();
    assert_eq!(told(repo, &[], true), state(repo));
    let sock = repo.join(".alt").join("daemon.sock");
    assert!(std::os::unix::net::UnixStream::connect(&sock).is_ok());
}
