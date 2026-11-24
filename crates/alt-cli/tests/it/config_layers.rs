//! The configuration an import keeps of a git repository and the
//! configuration `alt config` writes are two files: the next import
//! replaces the first and leaves the second, the second stands over the
//! first, and an export writes the first with the second written into it
//! as `git config` would.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::like_git::ok;

/// `alt` with nobody named by the environment: who commits is then what
/// the configuration says. Served by the daemon of the repository where
/// `daemon` says so.
fn alt(daemon: bool, repo: &Path, args: &[&str]) -> Output {
    let mut alt = crate::alt_command();
    for name in [
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
    ] {
        alt.env_remove(name);
    }
    match daemon {
        true => alt.env("ALT_DAEMON_IDLE_MS", "1500"),
        false => alt.env("ALT_NO_DAEMON", "1"),
    };
    alt.current_dir(repo).args(args).output().unwrap()
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

/// A git repository in `dir` that names who commits, with one commit.
fn source(dir: &Path) -> PathBuf {
    let source = dir.join("git");
    std::fs::create_dir(&source).unwrap();
    ok(git(&source, &["init", "-q", "-b", "main", "."]));
    for (key, value) in [
        ("user.name", "Source User"),
        ("user.email", "source@user.invalid"),
        ("core.filemode", "false"),
    ] {
        ok(git(&source, &["config", key, value]));
    }
    std::fs::write(source.join("a"), "a\n").unwrap();
    ok(git(&source, &["add", "a"]));
    ok(git(&source, &["commit", "-q", "-m", "one"]));
    source
}

fn set_here_stands_over_the_next_import(daemon: bool) {
    let dir = alt_os::temp::tempdir().unwrap();
    let (source, imported) = (source(dir.path()), dir.path().join("alt"));
    let alt = |repo: &Path, args: &[&str]| alt(daemon, repo, args);
    let value = |key: &str| ok(alt(&imported, &["config", key]));
    ok(alt(&source, &["import", imported.to_str().unwrap()]));
    let kept = imported.join(".alt/git-import/source/config");
    let theirs = || std::fs::read(source.join(".git/config")).unwrap();
    assert_eq!(std::fs::read(&kept).unwrap(), theirs());
    // the store's own file leads to the copy and sets nothing yet
    let led = "[include]\n\tpath = source/config\n[core]\n\trepositoryformatversion = 0\n";
    let own = std::fs::read_to_string(imported.join(".alt/git-import/config")).unwrap();
    assert_eq!(own, led);

    // one variable the git repository sets, one it does not
    ok(alt(&imported, &["config", "user.name", "Alt User"]));
    ok(alt(&imported, &["config", "core.quotepath", "false"]));
    assert_eq!(std::fs::read(&kept).unwrap(), theirs());
    let own = std::fs::read(imported.join(".alt/git-import/config")).unwrap();

    ok(git(&source, &["config", "user.name", "Source Two"]));
    ok(git(&source, &["config", "user.email", "two@user.invalid"]));
    ok(git(&source, &["config", "alias.st", "status"]));
    std::fs::write(source.join("b"), "b\n").unwrap();
    ok(git(&source, &["add", "b"]));
    ok(git(&source, &["commit", "-q", "-m", "two"]));
    ok(alt(&source, &["import", imported.to_str().unwrap()]));

    assert_eq!(std::fs::read(&kept).unwrap(), theirs());
    assert_eq!(
        std::fs::read(imported.join(".alt/git-import/config")).unwrap(),
        own
    );
    assert_eq!(value("user.name"), "Alt User\n");
    assert_eq!(value("core.quotepath"), "false\n");
    assert_eq!(value("user.email"), "two@user.invalid\n");
    assert_eq!(value("alias.st"), "status\n");
    assert_eq!(value("core.filemode"), "false\n");

    // who commits is who is in force
    std::fs::write(imported.join("c"), "c\n").unwrap();
    ok(alt(&imported, &["add", "c"]));
    ok(alt(&imported, &["commit", "-m", "three"]));
    let made = ok(alt(&imported, &["log", "--pretty=raw", "-n", "1"]));
    assert!(
        made.contains("\nauthor Alt User <two@user.invalid> "),
        "{made}"
    );

    // what the import kept is not to be unset from here
    let refused = alt(&imported, &["config", "--unset", "alias.st"]);
    assert!(!refused.status.success());
    let said = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(said.contains("imported git configuration"), "{said}");
    assert_eq!(value("alias.st"), "status\n");

    // the export is the git repository's file with the two variables
    // written in as git writes them
    let exported = dir.path().join("exported");
    ok(alt(&imported, &["export", exported.to_str().unwrap()]));
    let by_git = dir.path().join("by-git");
    std::fs::write(&by_git, theirs()).unwrap();
    let file = by_git.to_str().unwrap();
    ok(git(
        dir.path(),
        &["config", "--file", file, "user.name", "Alt User"],
    ));
    ok(git(
        dir.path(),
        &["config", "--file", file, "core.quotepath", "false"],
    ));
    assert_eq!(
        String::from_utf8(std::fs::read(exported.join(".git/config")).unwrap()).unwrap(),
        std::fs::read_to_string(&by_git).unwrap()
    );
    assert_eq!(ok(git(&exported, &["config", "user.name"])), "Alt User\n");
}

#[test]
fn what_is_set_here_stands_over_the_next_import() {
    set_here_stands_over_the_next_import(false);
}

#[test]
fn what_is_set_here_stands_over_the_next_import_through_the_daemon() {
    set_here_stands_over_the_next_import(true);
}

/// A git repository whose configuration includes files by relative
/// paths: one of the working tree, two of the git directory, of which
/// one includes another file of the tree, and one beside the tree. Its
/// `config.worktree` is in force as well.
fn including(dir: &Path) -> PathBuf {
    let source = source(dir);
    let write = |path: &str, text: &str| {
        let at = source.join(path);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, text).unwrap();
    };
    write(".gitconfig", "[x]\n\tfromtree = tree\n");
    write("from-deeper.cfg", "[x]\n\tfromdeeper = yes\n");
    write(
        ".git/extra.cfg",
        "[x]\n\tingit = ingit\n[include]\n\tpath = sub/deeper.cfg\n",
    );
    write(
        ".git/sub/deeper.cfg",
        "[x]\n\tdeeper = deeper\n[include]\n\tpath = ../../from-deeper.cfg\n",
    );
    write("../outside.cfg", "[x]\n\toutside = outside\n");
    write(
        ".git/config.worktree",
        "[x]\n\twt = fromwt\n[user]\n\tname = Tree User\n",
    );
    for path in [
        "../.gitconfig",
        "extra.cfg",
        "../../outside.cfg",
        "missing.cfg",
    ] {
        ok(git(&source, &["config", "--add", "include.path", path]));
    }
    ok(git(
        &source,
        &["config", "extensions.worktreeConfig", "true"],
    ));
    ok(git(&source, &["add", ".gitconfig", "from-deeper.cfg"]));
    ok(git(&source, &["commit", "-q", "-m", "settings"]));
    source
}

const INCLUDED: [&str; 7] = [
    "x.fromtree",
    "x.ingit",
    "x.deeper",
    "x.fromdeeper",
    "x.wt",
    "user.name",
    "user.email",
];

#[test]
fn what_the_configuration_included_is_read_from_where_it_is_now() {
    let dir = alt_os::temp::tempdir().unwrap();
    let source = including(dir.path());
    let imported = dir.path().join("deep/alt");
    std::fs::create_dir_all(&imported).unwrap();
    assert_eq!(ok(git(&source, &["config", "x.outside"])), "outside\n");

    let said = ok(alt(false, &source, &["import", imported.to_str().unwrap()]));
    let warned: Vec<&str> = said.lines().filter(|l| l.starts_with("warning:")).collect();
    assert_eq!(warned.len(), 1, "{said}");
    assert!(warned[0].starts_with("warning: 'x.outside' did not come along: "));
    for key in INCLUDED {
        let here = ok(alt(false, &imported, &["config", key]));
        assert_eq!(here, ok(git(&source, &["config", key])), "{key}");
    }
    assert!(
        !alt(false, &imported, &["config", "x.outside"])
            .status
            .success()
    );

    // the export has every file of the git directory the import kept
    ok(alt(false, &imported, &["config", "user.name", "Alt User"]));
    let exported = dir.path().join("deep/exported");
    ok(alt(
        false,
        &imported,
        &["export", exported.to_str().unwrap()],
    ));
    for file in ["extra.cfg", "sub/deeper.cfg"] {
        let read = |root: &Path| std::fs::read(root.join(".git").join(file)).unwrap();
        assert_eq!(read(&exported), read(&source), "{file}");
    }
    ok(git(&exported, &["checkout", "-q", "main", "--", "."]));
    assert_eq!(ok(git(&exported, &["config", "x.deeper"])), "deeper\n");
    assert_eq!(ok(git(&exported, &["config", "x.fromdeeper"])), "yes\n");
    // the tree's own file stood over `config`, so the name is set in it
    assert_eq!(ok(git(&exported, &["config", "user.name"])), "Alt User\n");
    let tree = std::fs::read_to_string(exported.join(".git/config.worktree")).unwrap();
    assert_eq!(tree, "[x]\n\twt = fromwt\n[user]\n\tname = Alt User\n");
}

#[test]
fn an_import_in_place_reads_all_the_git_repository_read() {
    let dir = alt_os::temp::tempdir().unwrap();
    let source = including(dir.path());
    let read: Vec<String> = INCLUDED
        .iter()
        .chain(&["x.outside"])
        .map(|key| ok(git(&source, &["config", key])))
        .collect();
    let said = ok(alt(false, &source, &["import", "."]));
    assert!(!said.contains("warning:"), "{said}");
    // the git directory is not what the paths are followed through
    std::fs::rename(source.join(".git"), dir.path().join("was.git")).unwrap();
    for (key, was) in INCLUDED.iter().chain(&["x.outside"]).zip(read) {
        assert_eq!(ok(alt(false, &source, &["config", key])), was, "{key}");
    }
}
