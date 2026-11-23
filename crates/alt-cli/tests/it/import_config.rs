//! After `alt import`, the configuration of the git repository goes on
//! holding in the store: who commits, how files compare, the remotes.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use crate::like_git::ok;

/// `alt` with nobody named by the environment: who commits is then what
/// the configuration says.
fn alt(repo: &Path, args: &[&str]) -> Output {
    let mut alt = crate::alt_command();
    for name in [
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
    ] {
        alt.env_remove(name);
    }
    alt.current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .args(args)
        .output()
        .unwrap()
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap()
}

/// A git repository with a configuration of its own and one commit, and
/// the directory it is imported into.
fn configured() -> (
    alt_os::temp::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let dir = alt_os::temp::tempdir().unwrap();
    let (source, imported) = (dir.path().join("git"), dir.path().join("alt"));
    std::fs::create_dir(&source).unwrap();
    ok(git(&source, &["init", "-q", "-b", "main", "."]));
    for (key, value) in [
        ("user.name", "Repo User"),
        ("user.email", "repo@user.invalid"),
        ("core.filemode", "false"),
        ("core.quotepath", "false"),
        ("remote.origin.url", "https://example.invalid/origin.git"),
        ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
        ("remote.mirror.url", "https://example.invalid/mirror.git"),
        (
            "remote.mirror.fetch",
            "+refs/heads/main:refs/remotes/mirror/main",
        ),
        ("branch.main.remote", "origin"),
        ("branch.main.merge", "refs/heads/main"),
    ] {
        ok(git(&source, &["config", key, value]));
    }
    std::fs::write(source.join("a"), "a\n").unwrap();
    ok(git(&source, &["add", "a"]));
    ok(git(&source, &["commit", "-q", "-m", "one"]));
    ok(alt(&source, &["import", imported.to_str().unwrap()]));
    (dir, source, imported)
}

#[test]
fn who_commits_is_who_the_git_repository_named() {
    let (_dir, _, imported) = configured();
    std::fs::write(imported.join("b"), "b\n").unwrap();
    ok(alt(&imported, &["add", "b"]));
    ok(alt(&imported, &["commit", "-m", "two"]));
    let made = ok(alt(&imported, &["log", "--pretty=raw", "-n", "1"]));
    assert!(
        made.contains("\nauthor Repo User <repo@user.invalid> "),
        "{made}"
    );
    assert!(
        made.contains("\ncommitter Repo User <repo@user.invalid> "),
        "{made}"
    );
}

#[test]
fn the_settings_read_as_git_reads_them_and_hold() {
    let (_dir, source, imported) = configured();
    let listed = ok(alt(&imported, &["config", "--list"]));
    assert_eq!(listed, ok(git(&source, &["config", "--local", "--list"])));
    assert_eq!(ok(alt(&imported, &["config", "user.name"])), "Repo User\n");
    // core.fileMode is off: a changed bit is no change
    let file = imported.join("a");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(ok(alt(&imported, &["status"])).contains("working tree clean"));
    // core.quotePath is off: a name outside ASCII is shown as it is
    std::fs::write(imported.join("é"), "e\n").unwrap();
    assert!(ok(alt(&imported, &["status"])).contains("\té\n"));
}

#[test]
fn the_remotes_are_the_remotes_of_the_store() {
    let (_dir, source, imported) = configured();
    let listed = ok(alt(&imported, &["remote", "list"]));
    let expected = "mirror\thttps://example.invalid/mirror.git\n\
                    origin\thttps://example.invalid/origin.git\n";
    assert_eq!(listed, expected);
    let json = ok(alt(&imported, &["remote", "list", "--json"]));
    assert!(
        json.contains("\"fetch\":\"+refs/heads/main:refs/remotes/mirror/main\""),
        "{json}"
    );

    // a remote the store has is the store's own: the next import leaves it
    ok(alt(&imported, &["remote", "remove", "origin"]));
    ok(alt(
        &imported,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/moved.git",
        ],
    ));
    ok(git(
        &source,
        &[
            "config",
            "remote.third.url",
            "https://example.invalid/third.git",
        ],
    ));
    let again = ok(alt(&source, &["import", imported.to_str().unwrap()]));
    assert!(
        again.contains("took remote 'third'\n") && !again.contains("'origin'"),
        "{again}"
    );
    let listed = ok(alt(&imported, &["remote", "list"]));
    assert!(
        listed.contains("origin\thttps://example.invalid/moved.git\n"),
        "{listed}"
    );
    assert!(
        listed.contains("third\thttps://example.invalid/third.git\n"),
        "{listed}"
    );
}

#[test]
fn an_export_gives_the_configuration_back_as_it_was() {
    let (dir, source, imported) = configured();
    let exported = dir.path().join("exported");
    ok(alt(&imported, &["export", exported.to_str().unwrap()]));
    let was = std::fs::read(source.join(".git/config")).unwrap();
    assert_eq!(std::fs::read(exported.join(".git/config")).unwrap(), was);
}
