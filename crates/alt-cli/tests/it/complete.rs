//! `alt complete`: what a shell is told may stand at the cursor.

use std::path::Path;
use std::process::Output;

use alt_refs::{RefChange, RefStore, RefTarget};

mod paths;
mod reading;
mod shell;

fn alt_with(cwd: &Path, args: &[&str], daemon: bool) -> Output {
    let mut cmd = crate::alt_command();
    cmd.current_dir(cwd)
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

fn alt(cwd: &Path, args: &[&str]) -> String {
    let done = alt_with(cwd, args, false);
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert!(done.status.success(), "{args:?}: {stderr}");
    String::from_utf8(done.stdout).unwrap()
}

/// The answer for `line`, whose last word the cursor is in. An answer is
/// never an error and never says anything on stderr.
fn asked(cwd: &Path, line: &str, daemon: bool) -> String {
    let words: Vec<&str> = line.split(' ').collect();
    let current = (words.len() - 1).to_string();
    let mut args = vec!["complete", "--shell", "zsh", "--current", &current, "--"];
    args.extend(words);
    let done = alt_with(cwd, &args, daemon);
    assert!(done.status.success(), "{line}: {:?}", done.status);
    assert_eq!(String::from_utf8_lossy(&done.stderr), "", "{line}");
    String::from_utf8(done.stdout).unwrap()
}

fn answer(cwd: &Path, line: &str) -> String {
    asked(cwd, line, false)
}

/// The candidates of `group` in `answer`, in the order they are given.
fn group<'a>(answer: &'a str, group: &str) -> Vec<&'a str> {
    let mut within = false;
    let mut found = Vec::new();
    for line in answer.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["group", name, _] => within = *name == group,
            ["nospace"] | ["files", _] | ["prefix", _] => {}
            [value, _] if within => found.push(*value),
            _ => {}
        }
    }
    found
}

fn groups(answer: &str) -> Vec<&str> {
    let lines = answer
        .lines()
        .filter_map(|line| line.strip_prefix("group\t"));
    lines.map(|rest| rest.split('\t').next().unwrap()).collect()
}

fn head(root: &Path) -> String {
    alt(root, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn set_ref(root: &Path, name: &str, oid: &str) {
    let oid = alt_git_codec::ObjectId::from_hex(oid.as_bytes()).unwrap();
    let change = RefChange {
        name: name.to_owned(),
        old: None,
        new: Some(RefTarget::Oid(oid)),
    };
    let mut refs = RefStore::open(root.join(".alt")).unwrap();
    refs.commit("test", 1_700_000_000_000, &[change]).unwrap();
}

/// A repository with branches, a tag, a remote with two branches fetched,
/// a notes ref, and a working tree with a changed and two untracked paths.
fn repository() -> alt_os::temp::TempDir {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    alt(root, &["init", "."]);
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "fn main() {}\n").unwrap();
    alt(root, &["add", "."]);
    alt(root, &["commit", "-m", "first commit"]);
    for branch in ["topic", "feature/login", "release/1.0", "hotfix/urgent"] {
        alt(root, &["branch", branch]);
    }
    alt(root, &["tag", "v1.0"]);
    alt(
        root,
        &["remote", "add", "origin", "https://example.invalid/r.git"],
    );
    let tip = head(root);
    set_ref(root, "refs/remotes/origin/main", &tip);
    set_ref(root, "refs/remotes/origin/dev", &tip);
    set_ref(root, "refs/notes/review", &tip);
    std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(root.join("new.txt"), "new\n").unwrap();
    std::fs::create_dir(root.join("fresh")).unwrap();
    std::fs::write(root.join("fresh/x.txt"), "x\n").unwrap();
    dir
}

const BRANCHES: [&str; 5] = [
    "feature/login",
    "hotfix/urgent",
    "main",
    "release/1.0",
    "topic",
];

#[test]
fn outside_a_repository_only_the_command_line_answers() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    let commands = answer(root, "alt ");
    assert_eq!(groups(&commands), ["commands"]);
    let names = group(&commands, "commands");
    assert!(names.contains(&"status") && names.contains(&"switch"));
    assert!(!names.contains(&"complete"), "{commands}");
    assert!(
        commands.contains("status\tShow the working tree status\n"),
        "{commands}"
    );
    let options = answer(root, "alt commit --a");
    assert!(group(&options, "options").contains(&"--amend"), "{options}");
    let options = answer(root, "alt merge --a");
    assert!(group(&options, "options").contains(&"--abort"), "{options}");
    assert_eq!(answer(root, "alt switch "), "");
    assert_eq!(answer(root, "alt init "), "files\tdir\n");
    assert_eq!(
        answer(root, "alt complete --shell="),
        "prefix\t--shell=\ngroup\tvalues\tvalue\nzsh\t\n"
    );
}

#[test]
fn a_repository_that_does_not_open_is_answered_like_none() {
    let dir = alt_os::temp::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".alt")).unwrap();
    assert_eq!(answer(dir.path(), "alt switch "), "");
    assert_eq!(groups(&answer(dir.path(), "alt ")), ["commands"]);
}

#[test]
fn branches_tags_and_remote_branches_are_offered_where_they_are_taken() {
    let dir = repository();
    let root = dir.path();
    let switch = answer(root, "alt switch ");
    assert_eq!(groups(&switch), ["branches"]);
    assert_eq!(group(&switch, "branches"), BRANCHES);
    assert_eq!(group(&answer(root, "alt merge to"), "branches"), BRANCHES);
    assert_eq!(group(&answer(root, "alt branch -d "), "branches"), BRANCHES);
    let remote = answer(root, "alt branch -r -d ");
    assert_eq!(groups(&remote), ["remote-branches"]);
    assert_eq!(
        group(&remote, "remote-branches"),
        ["origin/dev", "origin/main"]
    );
    assert_eq!(answer(root, "alt tag -d "), "group\ttags\ttag\nv1.0\t\n");
    assert_eq!(
        group(&answer(root, "alt flow feature finish "), "branches"),
        ["login"]
    );
    assert_eq!(
        group(&answer(root, "alt flow release finish "), "branches"),
        ["1.0"]
    );
    assert_eq!(answer(root, "alt flow feature start "), "");
}

#[test]
fn a_revision_is_a_name_or_the_start_of_a_commit_id() {
    let dir = repository();
    let root = dir.path();
    let show = answer(root, "alt show ");
    assert_eq!(
        groups(&show),
        ["branches", "tags", "remote-branches", "commits"]
    );
    assert_eq!(group(&show, "branches"), BRANCHES);
    assert_eq!(group(&show, "tags"), ["v1.0"]);
    assert_eq!(group(&show, "commits"), ["HEAD"]);
    let tip = head(root);
    let by_id = answer(root, &format!("alt show {}", &tip[..6]));
    assert_eq!(group(&by_id, "commits"), ["HEAD", tip.as_str()]);
    assert!(by_id.contains(&format!("{tip}\tfirst commit\n")), "{by_id}");
    // three letters are too few, and other words are no id
    let short = answer(root, &format!("alt show {}", &tip[..3]));
    assert_eq!(group(&short, "commits"), ["HEAD"]);
}

#[test]
fn remotes_refspecs_and_workspaces_come_with_what_they_are() {
    let dir = repository();
    let root = dir.path();
    assert_eq!(
        answer(root, "alt fetch "),
        "group\tremotes\tremote\norigin\thttps://example.invalid/r.git\n"
    );
    let push = answer(root, "alt push origin ");
    assert_eq!(group(&push, "branches"), BRANCHES);
    assert!(
        group(&push, "refs").contains(&"refs/notes/review"),
        "{push}"
    );
    assert_eq!(
        answer(root, "alt push origin main:"),
        "prefix\tmain:\ngroup\tremote-branches\tremote branch\ndev\t\nmain\t\n"
    );
    let forced = answer(root, "alt push origin +ma");
    assert!(
        forced.starts_with("prefix\t+\ngroup\tbranches\t"),
        "{forced}"
    );

    let other = alt_os::temp::tempdir().unwrap();
    let place = other.path().join("w");
    alt(
        root,
        &["workspace", "add", "side", place.to_str().unwrap(), "topic"],
    );
    let workspaces = answer(root, "alt --workspace ");
    assert_eq!(group(&workspaces, "workspaces"), ["default", "side"]);
    let side = workspaces.lines().find(|line| line.starts_with("side\t"));
    let side = side.unwrap();
    assert!(side.ends_with("w on topic"), "{side}");
    assert_eq!(
        group(&answer(root, "alt workspace remove "), "workspaces"),
        ["default", "side"]
    );
}

#[test]
fn configuration_keys_are_those_set_and_those_alt_reads() {
    let dir = repository();
    let root = dir.path();
    alt(root, &["config", "user.name", "tester"]);
    alt(root, &["config", "core.autocrlf", "false"]);
    let keys = answer(root, "alt config ");
    let keys = group(&keys, "config-keys");
    assert!(keys.contains(&"user.name"), "{keys:?}");
    assert!(keys.contains(&"core.quotePath"), "{keys:?}");
    let autocrlf = keys
        .iter()
        .filter(|key| key.eq_ignore_ascii_case("core.autocrlf"));
    assert_eq!(autocrlf.count(), 1, "{keys:?}");
}
