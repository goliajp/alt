//! What `alt config`, and the commands that keep what a branch follows,
//! write into the store's own configuration file: the lines of the one
//! variable or section, with every other byte of the file left, and a
//! value so that it reads back as it was given and sets nothing else.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::like_git::{alt, ok};

fn own(repo: &Path) -> PathBuf {
    repo.join(".alt/git-import/config")
}

fn text(repo: &Path) -> String {
    std::fs::read_to_string(own(repo)).unwrap()
}

/// What git reads `key` as in the store's own file.
fn git_reads(repo: &Path, key: &str) -> String {
    let file = own(repo);
    let read = Command::new("git")
        .args(["config", "--file", file.to_str().unwrap(), "--get", key])
        .output()
        .unwrap();
    ok(read)
}

fn repo() -> alt_os::temp::TempDir {
    let dir = alt_os::temp::tempdir().unwrap();
    ok(alt(dir.path(), &["init", "."]));
    std::fs::write(dir.path().join("f"), "f\n").unwrap();
    ok(alt(dir.path(), &["add", "f"]));
    ok(alt(dir.path(), &["commit", "-m", "one"]));
    std::fs::create_dir_all(own(dir.path()).parent().unwrap()).unwrap();
    dir
}

const BY_HAND: &str = "# kept\n[user]\n\tname = \"A ; B\" # said\n[alias]\n\
                       \tsay = \"!echo \\\"hi\\\"\"\n\n[core]\n\teditor = \"C:\\\\x\"\n";

#[test]
fn a_write_leaves_every_other_line_of_the_file_as_it_is() {
    let dir = repo();
    let repo = dir.path();
    std::fs::write(own(repo), BY_HAND).unwrap();
    ok(alt(repo, &["config", "other.key", "v"]));
    assert_eq!(text(repo), format!("{BY_HAND}[other]\n\tkey = v\n"));
    for (key, value) in [
        ("user.name", "A ; B\n"),
        ("alias.say", "!echo \"hi\"\n"),
        ("core.editor", "C:\\x\n"),
    ] {
        assert_eq!(ok(alt(repo, &["config", key])), value, "{key}");
        assert_eq!(git_reads(repo, key), value, "{key}");
    }
    ok(alt(repo, &["config", "other.key", "w"]));
    assert_eq!(text(repo), format!("{BY_HAND}[other]\n\tkey = w\n"));
    ok(alt(repo, &["config", "--unset", "other.key"]));
    assert_eq!(text(repo), BY_HAND);
    ok(alt(repo, &["status"]));
}

#[test]
fn a_value_reads_back_as_it_was_given_and_sets_nothing_else() {
    let dir = repo();
    let repo = dir.path();
    for value in [
        "A ; B",
        " led and trailed ",
        "say \"hi\" \\ there",
        "tab\there # no comment",
        "x\n[extensions]\n\tobjectformat = sha256",
    ] {
        ok(alt(repo, &["config", "user.nick", value]));
        assert_eq!(
            ok(alt(repo, &["config", "user.nick"])),
            format!("{value}\n")
        );
        assert_eq!(git_reads(repo, "user.nick"), format!("{value}\n"));
    }
    assert!(!text(repo).contains("\n[extensions]"), "{}", text(repo));
    let unset = alt(repo, &["config", "extensions.objectformat"]);
    assert!(!unset.status.success());
    ok(alt(repo, &["status"]));
    ok(alt(repo, &["log", "-n", "1"]));
}

#[test]
fn what_is_no_key_and_what_tells_the_format_of_the_store_is_not_written() {
    let dir = repo();
    let repo = dir.path();
    std::fs::write(own(repo), BY_HAND).unwrap();
    for key in [
        "user.ni\nck",
        "us er.name",
        "user.1name",
        "user.name]",
        "a]\n[extensions.objectformat",
        "user.sub\nsection.name",
        "extensions.objectformat",
        "Extensions.ObjectFormat",
        "core.repositoryFormatVersion",
    ] {
        let refused = alt(repo, &["config", key, "sha256"]);
        assert!(!refused.status.success(), "{key:?}");
        assert_eq!(text(repo), BY_HAND, "{key:?}");
    }
}

#[test]
fn a_branch_renamed_or_deleted_leaves_the_rest_of_the_file() {
    let dir = repo();
    let repo = dir.path();
    let follows = "\tremote = origin ; said\n\tmerge = refs/heads/side\n";
    let rest = "[user]\n\tname = \"A ; B\" # kept\n";
    let before = format!("# mine\n[branch \"side\"]\n{follows}{rest}");
    std::fs::write(own(repo), before).unwrap();
    ok(alt(repo, &["branch", "side"]));
    ok(alt(repo, &["branch", "-m", "side", "other"]));
    assert_eq!(
        text(repo),
        format!("# mine\n[branch \"other\"]\n{follows}{rest}")
    );
    ok(alt(repo, &["branch", "-d", "other"]));
    assert_eq!(text(repo), format!("# mine\n{rest}"));
    assert_eq!(ok(alt(repo, &["config", "user.name"])), "A ; B\n");
}

#[test]
fn a_write_gives_up_where_another_writer_holds_the_lock() {
    let dir = repo();
    let repo = dir.path();
    std::fs::write(own(repo), BY_HAND).unwrap();
    let lock = own(repo).with_extension("lock");
    std::fs::write(&lock, "theirs").unwrap();
    let refused = alt(repo, &["config", "other.key", "v"]);
    let said = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(!refused.status.success());
    assert!(said.contains("could not lock config file"), "{said}");
    assert_eq!(text(repo), BY_HAND);
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), "theirs");
    std::fs::remove_file(&lock).unwrap();
    ok(alt(repo, &["config", "other.key", "v"]));
    assert!(!lock.exists());
    // nothing but the file is left beside it
    let left = std::fs::read_dir(lock.parent().unwrap()).unwrap();
    let left: Vec<_> = left.map(|e| e.unwrap().file_name()).collect();
    assert_eq!(left, ["config"], "{left:?}");
}

#[test]
fn a_variable_with_several_values_is_changed_as_git_changes_it() {
    let dir = repo();
    let root = dir.path();
    let twin = root.join("twin");
    let several = "[a]\n\tb = 1\n\tb = 2\n\tc = x\n";
    std::fs::write(own(root), several).unwrap();
    std::fs::write(&twin, several).unwrap();
    let steps: [(&[&str], i32); 10] = [
        // one value does not take the place of several
        (&["a.b", "3"], 5),
        (&["--unset", "a.b"], 5),
        (&["--add", "a.b", "3"], 0),
        (&["a.b", "4"], 5),
        (&["--replace-all", "a.b", "5"], 0),
        (&["a.b", "6"], 0),
        (&["--add", "a.c", "y"], 0),
        (&["--unset-all", "a.c"], 0),
        (&["--replace-all", "a.d", "new"], 0),
        (&["--unset-all", "a.b"], 0),
    ];
    for (args, code) in steps {
        let ours = alt(root, &[&["config"], args].concat());
        let theirs = Command::new("git")
            .args(["config", "--file", twin.to_str().unwrap()])
            .args(args)
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&ours.stderr).into_owned();
        assert_eq!(theirs.status.code(), Some(code), "git {args:?}");
        assert_eq!(ours.status.code(), Some(code), "{args:?}: {said}");
        let git_said = String::from_utf8_lossy(&theirs.stderr).into_owned();
        assert!(said.starts_with(&git_said), "{args:?}: {said}");
        assert_eq!(
            text(root),
            std::fs::read_to_string(&twin).unwrap(),
            "{args:?}"
        );
    }
}
