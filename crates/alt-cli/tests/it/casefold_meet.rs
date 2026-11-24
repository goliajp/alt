//! A file and a directory whose names differ in case alone, on a file
//! system that takes such names for one: a tree that holds both, and a
//! checkout that takes one away and brings the other.

use std::path::{Path, PathBuf};

use alt_os::temp::TempDir;

use crate::casefold::{clean, code, folded, place};
use crate::casefold_dirs::{read, told};
use crate::casefold_wider::cut;
use crate::like_git::{alt, alt_daemon, git, heads, index, ok};

pub(crate) const MEET: &str = "meet in one name here";

pub(crate) fn put(root: &Path, file: &str, content: &str) {
    let path = root.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A git repository, made where case is told apart: `only-file` has the
/// file `d`, `only-dir` the file `D/y`, `both` has both, and `deep` has
/// such pairs further down and with a symbolic link.
pub(crate) fn source() -> TempDir {
    let made = alt_os::temp::tempdir().unwrap();
    let root = made.path();
    let commit = |message: &str| {
        ok(git(root, &["add", "-A"]));
        ok(git(root, &["commit", "-q", "-m", message]));
    };
    ok(git(root, &["init", "-q", "-b", "main", "."]));
    put(root, "keep", "base\n");
    commit("base");
    ok(git(root, &["switch", "-q", "-c", "only-file"]));
    put(root, "d", "file\n");
    commit("file");
    ok(git(root, &["switch", "-q", "-c", "both"]));
    put(root, "D/y", "y\n");
    commit("both");
    ok(git(root, &["switch", "-q", "-c", "only-dir", "main"]));
    put(root, "D/y", "y\n");
    commit("dir");
    ok(git(root, &["branch", "pick"]));
    ok(git(root, &["switch", "-q", "-c", "deep", "main"]));
    put(root, "a/B/c", "c\n");
    put(root, "A/b", "b\n");
    put(root, "l/z", "z\n");
    std::os::unix::fs::symlink("keep", root.join("L")).unwrap();
    commit("deep");
    // two lines that change `keep` each its own way, one with the
    // directory and one that brings the file
    ok(git(root, &["switch", "-q", "-c", "ours", "only-dir"]));
    put(root, "keep", "ours\n");
    commit("ours");
    ok(git(root, &["switch", "-q", "-c", "theirs", "only-file"]));
    put(root, "keep", "theirs\n");
    commit("theirs");
    made
}

/// The store an import of `source` at `branch` makes on the file system
/// that ignores case, and what the import said.
pub(crate) fn imported(source: &Path, branch: &str) -> (TempDir, PathBuf, String) {
    ok(git(source, &["switch", "-q", branch]));
    let held = alt_os::temp::tempdir_in(place()).unwrap();
    let repo = held.path().join("r");
    let said = ok(alt(source, &["import", repo.to_str().unwrap()]));
    (held, repo, said)
}

pub(crate) fn nothing_pending(repo: &Path) {
    assert_eq!(ok(alt(repo, &["operation", "pending"])), "");
    assert!(!repo.join(".alt/names-asked").exists());
}

/// Each command fails saying which two paths meet, as it fails over a
/// file that is not tracked, and leaves everything as it is.
pub(crate) fn refused(repo: &Path, commands: &[&[&str]], first: &str, second: &str) {
    let (at, staged, tree) = (heads(repo), index(repo), folded(repo));
    for command in commands {
        let out = alt(repo, command);
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        let fails = if matches!(command[0], "merge" | "cherry-pick") {
            75
        } else {
            1
        };
        assert_eq!(code(&out), Some(fails), "{command:?}: {said}");
        let named = format!("'{first}' and '{second}' {MEET}");
        assert!(said.contains(&named), "{command:?}: {said}");
        assert_eq!(heads(repo), at, "{command:?}");
        assert_eq!(index(repo), staged, "{command:?}");
        assert_eq!(folded(repo), tree, "{command:?}");
        nothing_pending(repo);
        clean(repo, false);
    }
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn an_import_of_a_file_and_a_directory_of_one_name_leaves_what_a_clone_leaves() {
    let source = source();
    let both = (vec!["D/y"], vec!["'D/y'", "'d'"]);
    let deep = (vec!["A/b", "L"], vec!["'A/b'", "'L'", "'a/B/c'", "'l/z'"]);
    for (branch, (gone, met)) in [("both", both), ("deep", deep)] {
        let (held, repo, said) = imported(source.path(), branch);
        let cloned = held.path().join("g");
        let clone = git(
            source.path(),
            &["clone", "-q", ".", cloned.to_str().unwrap()],
        );
        assert!(clone.status.success());
        // every path that met another is named; git names them where
        // the two are side by side in one directory
        let warned = said.find("warning: the following paths have collided");
        let ours = &said[warned.unwrap_or_else(|| panic!("{said}"))..];
        let paths = |text: &str| -> Vec<String> {
            let named = text.lines().filter(|l| l.starts_with("  '"));
            let mut named: Vec<String> = named.map(|l| l.trim().to_owned()).collect();
            named.sort();
            named
        };
        assert_eq!(paths(ours), met, "{branch}");
        if branch == "both" {
            let theirs = String::from_utf8_lossy(&clone.stderr).into_owned();
            assert_eq!(paths(&theirs), met);
        }
        // the files of the clone, and the paths that have none told as gone
        nothing_pending(&repo);
        assert_eq!(folded(&repo), folded(&cloned), "{branch}");
        let lost: Vec<String> = gone.iter().map(|path| format!("deleted: {path}")).collect();
        assert_eq!(told(&ok(alt(&repo, &["status"]))), lost, "{branch}");
        assert_eq!(told(&ok(alt_daemon(&repo, &["status"]))), lost, "{branch}");
        let porcelain: String = gone.iter().map(|path| format!(" D {path}\n")).collect();
        assert_eq!(ok(git(&cloned, &["status", "--porcelain"])), porcelain);
        assert_eq!(index(&repo), ok(git(&cloned, &["ls-files", "-s"])));
        // the store is one to work in
        put(&repo, "keep", "worked in\n");
        ok(alt(&repo, &["add", "keep"]));
        ok(alt(&repo, &["commit", "-m", "worked in"]));
        assert_eq!(told(&ok(alt(&repo, &["status"]))), lost);
        nothing_pending(&repo);
    }
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_tree_with_a_file_and_a_directory_of_one_name_is_not_checked_out() {
    let source = source();
    // neither is there yet
    let (_held, repo, _) = imported(source.path(), "main");
    refused(&repo, &[&["switch", "both"]], "d", "D/y");
    refused(&repo, &[&["switch", "deep"]], "A/b", "a/B/c");
    let served = alt_daemon(&repo, &["switch", "both"]);
    assert_eq!(code(&served), Some(1));
    assert!(String::from_utf8_lossy(&served.stderr).contains(MEET));
    nothing_pending(&repo);
    // the file is there, and the directory would come
    let (_held, repo, _) = imported(source.path(), "only-file");
    let commands: [&[&str]; 6] = [
        &["switch", "both"],
        &["merge", "only-dir"],
        &["cherry-pick", "pick"],
        &["reset", "--hard", "both"],
        &["restore", "--source", "both", "--", "D/y"],
        &["checkout", "both", "--", "D/y"],
    ];
    refused(&repo, &commands, "D/y", "d");
    assert_eq!(read(&repo, "d"), "file\n");
    // the directory is there, and the file would come
    let (_held, repo, _) = imported(source.path(), "only-dir");
    let commands: [&[&str]; 4] = [
        &["switch", "both"],
        &["merge", "only-file"],
        &["reset", "--hard", "both"],
        &["checkout", "only-file", "--", "d"],
    ];
    refused(&repo, &commands, "d", "D/y");
    assert_eq!(read(&repo, "D/y"), "y\n");
    // a change put away where the directory is does not come back where the file is
    put(&repo, "D/y", "changed\n");
    ok(alt(&repo, &["stash"]));
    ok(alt(&repo, &["switch", "only-file"]));
    refused(&repo, &[&["stash", "pop"]], "D/y", "d");
    assert!(ok(alt(&repo, &["stash", "list"])).contains("stash@{0}"));
    ok(alt(&repo, &["switch", "only-dir"]));
    ok(alt(&repo, &["stash", "pop"]));
    assert_eq!(read(&repo, "D/y"), "changed\n");
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_file_gives_way_to_a_directory_of_its_name_and_the_directory_to_the_file() {
    let source = source();
    let (held, repo, _) = imported(source.path(), "main");
    let cloned = held.path().join("g");
    ok(git(
        source.path(),
        &["clone", "-q", ".", cloned.to_str().unwrap()],
    ));
    // git has the index of each branch; of the files it is not asked,
    // since it leaves the file in place where the directory comes
    let same = |at: &str| {
        let files: &[(&str, &str)] = match at {
            "only-file" => &[("d", "file\n"), ("keep", "base\n")],
            "only-dir" => &[("d/y", "y\n"), ("keep", "base\n")],
            _ => &[("keep", "base\n")],
        };
        let files: Vec<_> = files
            .iter()
            .map(|(n, c)| (n.to_string(), c.to_string(), false))
            .collect();
        assert_eq!(folded(&repo), files, "{at}");
        assert_eq!(index(&repo), ok(git(&cloned, &["ls-files", "-s"])), "{at}");
        clean(&repo, false);
        nothing_pending(&repo);
    };
    for branch in [
        "only-file",
        "only-dir",
        "only-file",
        "only-dir",
        "main",
        "only-dir",
    ] {
        ok(alt(&repo, &["switch", branch]));
        ok(git(&cloned, &["switch", "-q", branch]));
        same(branch);
    }
    // taken back, and made again by a reset
    ok(alt(&repo, &["switch", "only-file"]));
    ok(alt(&repo, &["undo"]));
    same("only-dir");
    assert_eq!(read(&repo, "D/y"), "y\n");
    ok(alt(&repo, &["switch", "-c", "moved"]));
    ok(git(&cloned, &["switch", "-q", "-c", "moved"]));
    for to in ["only-file", "only-dir"] {
        ok(alt(&repo, &["reset", "--hard", to]));
        ok(git(&cloned, &["reset", "-q", "--hard", to]));
        same(to);
        ok(alt(&repo, &["undo"]));
        ok(alt(&repo, &["reset", "--hard", to]));
        same(to);
    }
    // through the daemon as by the command itself
    ok(alt_daemon(&repo, &["switch", "only-file"]));
    assert_eq!(read(&repo, "d"), "file\n");
    ok(alt_daemon(&repo, &["switch", "only-dir"]));
    assert_eq!(read(&repo, "D/y"), "y\n");
    clean(&repo, true);
}

/// Cuts `command` short at each of its points; after the recovery of what
/// it left, the tree is that of `from` or that of `to`, whole.
fn cut_at_each_point(repo: &Path, command: &[&str], from: &str, to: &str) {
    let list = repo.join(".alt/points");
    let whole = cut(repo, command, &format!("list:{}", list.display()));
    assert_eq!(code(&whole), Some(0));
    let points = std::fs::read_to_string(&list).unwrap().lines().count();
    std::fs::remove_file(&list).unwrap();
    assert!(points > 3, "{points}");
    let after = index(repo);
    ok(alt(repo, &["switch", from]));
    let before = index(repo);
    for at in 1..=points {
        let killed = cut(repo, command, &at.to_string());
        assert_eq!(code(&killed), None, "point {at}");
        let pending = ok(alt(repo, &["operation", "pending"]));
        if let Some(intent) = pending.lines().last() {
            ok(alt(
                repo,
                &["operation", "recover", intent.split(' ').next().unwrap()],
            ));
        }
        assert_eq!(ok(alt(repo, &["operation", "pending"])), "", "point {at}");
        let now = index(repo);
        assert!(now == before || now == after, "point {at}");
        match now.contains("\td\n") {
            true => assert_eq!(read(repo, "d"), "file\n", "point {at}"),
            false => assert_eq!(read(repo, "D/y"), "y\n", "point {at}"),
        }
        clean(repo, false);
        if now == after {
            ok(alt(repo, &["switch", from]));
        }
    }
    ok(alt(repo, &["switch", to]));
    clean(repo, false);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_file_and_a_directory_of_one_name_cut_short_in_each_other_s_place_are_one_or_the_other() {
    let source = source();
    let (_held, repo, _) = imported(source.path(), "only-file");
    cut_at_each_point(&repo, &["switch", "only-dir"], "only-file", "only-dir");
    cut_at_each_point(&repo, &["switch", "only-file"], "only-dir", "only-file");
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn an_import_cut_short_while_it_writes_the_tree_is_finished_as_the_import_would_have() {
    let source = source();
    ok(git(source.path(), &["switch", "-q", "both"]));
    let held = alt_os::temp::tempdir_in(place()).unwrap();
    let list = held.path().join("points");
    let first = held.path().join("whole");
    let whole = cut(
        source.path(),
        &["import", first.to_str().unwrap()],
        &format!("list:{}", list.display()),
    );
    assert_eq!(code(&whole), Some(0));
    let points = std::fs::read_to_string(&list).unwrap().lines().count();
    let mut finished = 0;
    for at in 1..=points {
        let repo = held.path().join(format!("r{at}"));
        let killed = cut(
            source.path(),
            &["import", repo.to_str().unwrap()],
            &at.to_string(),
        );
        assert_eq!(code(&killed), None, "point {at}");
        let pending = alt(&repo, &["operation", "pending"]);
        let pending = String::from_utf8_lossy(&pending.stdout).into_owned();
        let Some(intent) = pending.lines().last() else {
            continue;
        };
        ok(alt(
            &repo,
            &["operation", "recover", intent.split(' ').next().unwrap()],
        ));
        finished += 1;
        nothing_pending(&repo);
        assert_eq!(read(&repo, "d"), "file\n", "point {at}");
        assert_eq!(
            told(&ok(alt(&repo, &["status"]))),
            ["deleted: D/y"],
            "point {at}"
        );
        put(&repo, "keep", "worked in\n");
        ok(alt(&repo, &["add", "keep"]));
        ok(alt(&repo, &["commit", "-m", "worked in"]));
    }
    assert!(finished > 0, "no point fell within the checkout");
}
