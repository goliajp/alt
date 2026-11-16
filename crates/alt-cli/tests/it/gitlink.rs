//! Submodule entries in the working tree, as git treats them: the
//! directory of one is made by a checkout, a missing one is a deletion,
//! and a repository of its own that is named to `add` becomes one.

use std::path::Path;
use std::process::{Command, Output};

use super::diff::{alt, ok};

/// `git` with fixed names and dates, so that the same steps give the same
/// commit wherever they are taken.
pub(super) fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .args(args)
        .output()
        .unwrap()
}

/// A git repository whose commit holds `a.txt` and a submodule entry at
/// `sub`, and the alt working tree imported from it.
pub(super) fn pair() -> (alt_os::temp::TempDir, alt_os::temp::TempDir) {
    let (g, a) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let src = g.path();
    ok(git(src, &["init", "-q", "-b", "main", "."]));
    std::fs::write(src.join("a.txt"), "one\n").unwrap();
    ok(git(src, &["add", "a.txt"]));
    let entry = "160000,1111111111111111111111111111111111111111,sub";
    ok(git(src, &["update-index", "--add", "--cacheinfo", entry]));
    ok(git(src, &["commit", "-qm", "base"]));
    ok(alt(src, &["import", a.path().to_str().unwrap()]));
    (g, a)
}

/// `alt status`, `diff` and `diff --cached` say what git says.
pub(super) fn agree(g: &Path, a: &Path, at: &str) -> String {
    for args in [&["diff"][..], &["diff", "--cached"]] {
        let theirs = ok(git(g, &[&["-c", "core.pager=cat"][..], args].concat()));
        assert_eq!(ok(alt(a, args)), theirs, "{args:?} {at}");
    }
    let porcelain = ok(git(g, &["status", "--porcelain", "-uno"]));
    let status = ok(alt(a, &["status", "-uno"]));
    let lines: Vec<String> = porcelain
        .lines()
        .map(|line| {
            let what = match &line[..2] {
                " D" | "D " => "deleted:    ",
                " M" | "M " => "modified:   ",
                "A " => "new file:   ",
                " T" | "T " => "typechange: ",
                other => panic!("{other}"),
            };
            format!("\t{what}{}", &line[3..])
        })
        .collect();
    for line in &lines {
        assert!(status.contains(line.as_str()), "{at}: {status}");
    }
    assert_eq!(status.matches('\t').count(), lines.len(), "{at}: {status}");
    porcelain
}

#[test]
fn a_submodule_entry_without_its_directory_is_deleted_as_for_git() {
    let (g, a) = pair();
    let (g, a) = (g.path(), a.path());
    assert!(a.join("sub").is_dir(), "made by the import");
    std::fs::create_dir(g.join("sub")).unwrap();
    assert_eq!(agree(g, a, "an empty directory"), "");
    for root in [g, a] {
        std::fs::write(root.join("sub/file"), "not this repository's\n").unwrap();
    }
    assert_eq!(agree(g, a, "a directory with a file"), "");
    for root in [g, a] {
        std::fs::remove_dir_all(root.join("sub")).unwrap();
    }
    assert_eq!(agree(g, a, "no directory"), " D sub\n");

    // no switch is refused over it, and none brings it back
    ok(git(g, &["switch", "-q", "-c", "other"]));
    ok(alt(a, &["switch", "-c", "other"]));
    assert_eq!(agree(g, a, "on another branch"), " D sub\n");
    assert!(!a.join("sub").exists());
    // staged with everything else, the entry goes
    ok(git(g, &["add", "."]));
    ok(alt(a, &["add", "."]));
    assert_eq!(agree(g, a, "staged"), "D  sub\n");
    ok(git(g, &["commit", "-qm", "without"]));
    ok(alt(a, &["commit", "-m", "without"]));
    // a checkout of a tree that has the entry makes its directory
    ok(git(g, &["switch", "-q", "main"]));
    ok(alt(a, &["switch", "main"]));
    assert!(g.join("sub").is_dir() && a.join("sub").is_dir());
    assert_eq!(agree(g, a, "back"), "");
}

/// A repository with one commit at `at`, the same commit wherever it is made.
pub(super) fn embed(at: &Path) {
    std::fs::create_dir_all(at).unwrap();
    ok(git(at, &["init", "-q", "-b", "main", "."]));
    std::fs::write(at.join("e.txt"), "its own\n").unwrap();
    ok(git(at, &["add", "e.txt"]));
    ok(git(at, &["commit", "-qm", "its own"]));
}

#[test]
fn add_stages_a_repository_of_its_own_as_a_submodule_as_git_does() {
    let (g, a) = pair();
    let (g, a) = (g.path(), a.path());
    std::fs::create_dir(g.join("sub")).unwrap();
    for root in [g, a] {
        embed(&root.join("emb"));
        embed(&root.join("deep/er/emb"));
        std::fs::write(root.join("deep/beside.txt"), "beside\n").unwrap();
    }
    // named by itself, and found beneath a named directory
    ok(git(g, &["add", "emb"]));
    assert_eq!(ok(alt(a, &["add", "emb"])), "staged 1 path(s)\n");
    assert_eq!(agree(g, a, "named"), "A  emb\n");
    ok(git(g, &["add", "deep"]));
    ok(alt(a, &["add", "deep/"]));
    assert_eq!(
        agree(g, a, "beneath a directory"),
        "A  deep/beside.txt\nA  deep/er/emb\nA  emb\n"
    );
    // what is inside is the submodule's from now on
    for named in ["emb/e.txt", "deep/er/emb/e.txt"] {
        assert!(!git(g, &["add", named]).status.success(), "{named}");
        assert!(!alt(a, &["add", named]).status.success(), "{named}");
    }
    // the trees committed are the same: the entries name the same commits
    ok(git(g, &["commit", "-qm", "embedded"]));
    ok(alt(a, &["commit", "-m", "embedded"]));
    let tree = ok(git(g, &["rev-parse", "HEAD^{tree}"]));
    let log = ok(alt(a, &["log", "-n", "1", "--json"]));
    assert!(
        log.contains(&format!("\"tree\":\"{}\"", tree.trim())),
        "{log}"
    );
    assert_eq!(agree(g, a, "committed"), "");
    // another commit checked out in it shows, and is staged, as a change
    for root in [g, a] {
        std::fs::write(root.join("emb/e.txt"), "more\n").unwrap();
        ok(git(&root.join("emb"), &["commit", "-qam", "more"]));
    }
    assert_eq!(agree(g, a, "moved on"), " M emb\n");
    ok(git(g, &["add", "."]));
    ok(alt(a, &["add", "."]));
    assert_eq!(agree(g, a, "moved on and staged"), "M  emb\n");
}

#[test]
fn another_type_of_entry_at_a_path_is_a_change_of_type_as_for_git() {
    let (g, a) = pair();
    let (g, a) = (g.path(), a.path());
    std::fs::remove_dir(a.join("sub")).unwrap();
    for root in [g, a] {
        // a file where the submodule was, a symlink where a file was
        std::fs::write(root.join("sub"), "a file now\n").unwrap();
        std::fs::remove_file(root.join("a.txt")).unwrap();
        std::os::unix::fs::symlink("sub", root.join("a.txt")).unwrap();
    }
    assert_eq!(agree(g, a, "in the tree"), " T a.txt\n T sub\n");
    ok(git(g, &["add", "."]));
    ok(alt(a, &["add", "."]));
    assert_eq!(agree(g, a, "staged"), "T  a.txt\nT  sub\n");
}

#[test]
fn a_submodule_entry_without_its_directory_stops_no_switch_and_no_merge() {
    let (g, a) = pair();
    let (g, a) = (g.path(), a.path());
    std::fs::create_dir(g.join("sub")).unwrap();
    // a branch at the same commit, and one a commit further
    ok(git(g, &["branch", "other"]));
    ok(alt(a, &["branch", "other"]));
    ok(git(g, &["switch", "-q", "-c", "third"]));
    ok(alt(a, &["switch", "-c", "third"]));
    for root in [g, a] {
        std::fs::write(root.join("b.txt"), "two\n").unwrap();
    }
    ok(git(g, &["add", "b.txt"]));
    ok(alt(a, &["add", "b.txt"]));
    ok(git(g, &["commit", "-qm", "third"]));
    ok(alt(a, &["commit", "-m", "third"]));
    ok(git(g, &["switch", "-q", "main"]));
    ok(alt(a, &["switch", "main"]));
    for root in [g, a] {
        std::fs::remove_dir(root.join("sub")).unwrap();
    }
    // the missing directory is nothing a checkout would write over, and
    // nothing it makes again where both sides have the entry
    ok(git(g, &["switch", "-q", "other"]));
    ok(alt(a, &["switch", "other"]));
    assert_eq!(agree(g, a, "on a branch that was there"), " D sub\n");
    ok(git(g, &["merge", "-q", "third"]));
    ok(alt(a, &["merge", "third"]));
    assert_eq!(agree(g, a, "merged"), " D sub\n");
    for root in [g, a] {
        assert!(root.join("b.txt").is_file() && !root.join("sub").exists());
    }
}
