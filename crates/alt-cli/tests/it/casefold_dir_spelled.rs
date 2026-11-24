//! On a file system that takes names differing in case for one, a
//! checkout leaves each directory spelled as git leaves it: a directory
//! all of whose files leave is gone, and is made again under the
//! spelling of the first path written into it; one that keeps a file
//! keeps its spelling.

use std::path::{Path, PathBuf};

use alt_os::temp::TempDir;

use crate::casefold::{clean, code, place, spelled};
use crate::casefold_meet::put;
use crate::casefold_wider::cut;
use crate::like_git::{alt, git, index, ok};

/// Each branch with the files of its tree, beside `keep`.
const BRANCHES: [(&str, &[(&str, &str)]); 16] = [
    ("la", &[("dir/a", "P")]),
    ("ua", &[("Dir/a", "P")]),
    ("uq", &[("Dir/a", "Q")]),
    ("fx", &[("Foo/a", "S")]),
    ("fy", &[("foo/b", "S")]),
    ("c1", &[("d/x", "1"), ("d/keep", "2")]),
    ("c2", &[("D/x", "1"), ("d/keep", "2")]),
    ("c3", &[("D/x", "1"), ("D/keep", "2")]),
    ("n1", &[("a/b/c", "1")]),
    ("n2", &[("A/b/c", "1")]),
    ("n3", &[("a/B/c", "1")]),
    ("f1", &[("d", "F")]),
    ("f2", &[("D/y", "F")]),
    ("f3", &[("D/y", "G")]),
    ("m1", &[("d/a", "M")]),
    ("m2", &[("d/b", "M"), ("D/c", "N")]),
];

/// A git repository made where case is told apart, with the branches
/// above, and the two repositories it becomes on the file system that
/// ignores case: the store an import makes of it, and a clone by git.
fn both() -> (TempDir, PathBuf, PathBuf) {
    let made = alt_os::temp::tempdir().unwrap();
    let source = made.path();
    ok(git(source, &["init", "-q", "-b", "main", "."]));
    put(source, "keep", "k\n");
    ok(git(source, &["add", "-A"]));
    ok(git(source, &["commit", "-q", "-m", "base"]));
    for (branch, files) in BRANCHES {
        ok(git(source, &["switch", "-q", "-c", branch, "main"]));
        for (file, content) in files {
            put(source, file, &format!("{content}\n"));
        }
        ok(git(source, &["add", "-A"]));
        ok(git(source, &["commit", "-q", "-m", branch]));
        ok(git(source, &["switch", "-q", "main"]));
        ok(git(source, &["clean", "-qfd"]));
    }
    let held = alt_os::temp::tempdir_in(place()).unwrap();
    let (repo, cloned) = (held.path().join("r"), held.path().join("g"));
    ok(alt(source, &["import", repo.to_str().unwrap()]));
    ok(git(source, &["clone", "-q", ".", cloned.to_str().unwrap()]));
    for (branch, _) in BRANCHES {
        let from = format!("origin/{branch}");
        ok(git(&cloned, &["branch", "-q", "--no-track", branch, &from]));
    }
    (held, repo, cloned)
}

fn listed(cloned: &Path) -> String {
    ok(git(cloned, &["ls-files", "-s"]))
}

/// Both switch to each of `branches` in turn. After each switch the two
/// trees spell every name alike; taken back, the tree of alt is spelled
/// as it was, and made again as git has it.
fn walk(repo: &Path, cloned: &Path, branches: &[&str]) {
    for branch in branches {
        let before = (spelled(repo), index(repo));
        ok(alt(repo, &["switch", branch]));
        ok(git(cloned, &["switch", "-q", branch]));
        assert_eq!(spelled(repo), spelled(cloned), "to {branch}");
        assert_eq!(index(repo), listed(cloned), "to {branch}");
        clean(repo, false);
        ok(alt(repo, &["undo"]));
        assert_eq!((spelled(repo), index(repo)), before, "back from {branch}");
        clean(repo, false);
        ok(alt(repo, &["undo"]));
        assert_eq!(spelled(repo), spelled(cloned), "again to {branch}");
        assert_eq!(index(repo), listed(cloned), "again to {branch}");
        clean(repo, false);
    }
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_directory_all_of_whose_files_leave_takes_the_spelling_of_what_comes() {
    let (_held, repo, cloned) = both();
    // the same file under a directory spelled otherwise, with the same
    // content and with other content, and another file of that content
    walk(
        &repo,
        &cloned,
        &["la", "ua", "la", "uq", "la", "main", "ua", "main"],
    );
    walk(&repo, &cloned, &["fx", "fy", "fx", "main", "fy", "main"]);
    walk(
        &repo,
        &cloned,
        &["n1", "n2", "n3", "n1", "n3", "n2", "main"],
    );
    // a file that only moves within its directory, beside one that
    // comes under another spelling of the directory and is written first
    walk(&repo, &cloned, &["m1", "m2", "m1", "main", "m2", "main"]);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_directory_that_keeps_a_file_keeps_its_spelling() {
    let (_held, repo, cloned) = both();
    walk(
        &repo,
        &cloned,
        &["c1", "c2", "c1", "c3", "c1", "c2", "c3", "c2"],
    );
    // the tree spells the directory in two ways, and the files have it
    // as the branch before left it; taken away and brought back, they
    // have it as a switch of git to the branch makes it
    assert!(spelled(&repo).contains(&"d".to_owned()));
    ok(alt(&repo, &["switch", "main"]));
    ok(git(&cloned, &["switch", "-q", "main"]));
    assert_eq!(spelled(&repo), spelled(&cloned));
    ok(alt(&repo, &["undo"]));
    ok(git(&cloned, &["switch", "-q", "c2"]));
    assert_eq!(spelled(&repo), spelled(&cloned));
    assert!(spelled(&repo).contains(&"D".to_owned()));
    assert_eq!(index(&repo), listed(&cloned));
    clean(&repo, false);
    ok(alt(&repo, &["switch", "main"]));
    ok(git(&cloned, &["switch", "-q", "main"]));
    // a file that is not tracked keeps the directory too
    for root in [&repo, &cloned] {
        ok(alt(&repo, &["switch", "la"]));
        ok(git(&cloned, &["switch", "-q", "la"]));
        put(root, "dir/mine", "m\n");
    }
    ok(alt(&repo, &["switch", "ua"]));
    ok(git(&cloned, &["switch", "-q", "ua"]));
    assert_eq!(spelled(&repo), spelled(&cloned));
    assert_eq!(index(&repo), listed(&cloned));
    ok(alt(&repo, &["switch", "main"]));
    ok(git(&cloned, &["switch", "-q", "main"]));
    assert_eq!(spelled(&repo), spelled(&cloned));
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_file_and_a_directory_of_its_name_give_way_under_the_spelling_of_each() {
    let (_held, repo, cloned) = both();
    walk(
        &repo,
        &cloned,
        &["f1", "f2", "f1", "f3", "f1", "main", "f2", "f3", "main"],
    );
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_switch_that_respells_a_directory_cut_short_ends_with_the_names_of_a_branch() {
    let (_held, repo, cloned) = both();
    for (from, to) in [("la", "ua"), ("c1", "c3"), ("fx", "fy")] {
        ok(git(&cloned, &["switch", "-q", to]));
        let at_to = (spelled(&cloned), listed(&cloned));
        ok(git(&cloned, &["switch", "-q", from]));
        let at_from = (spelled(&cloned), listed(&cloned));
        assert_ne!(at_to.0, at_from.0);
        ok(alt(&repo, &["switch", from]));
        let list = repo.join(".alt/points");
        let whole = cut(&repo, &["switch", to], &format!("list:{}", list.display()));
        assert_eq!(code(&whole), Some(0));
        let points = std::fs::read_to_string(&list).unwrap().lines().count();
        std::fs::remove_file(&list).unwrap();
        assert!(points > 3, "{points}");
        ok(alt(&repo, &["switch", from]));
        let mut finished = 0;
        for at in 1..=points {
            let killed = cut(&repo, &["switch", to], &at.to_string());
            assert_eq!(code(&killed), None, "{from} to {to}, point {at}");
            let pending = ok(alt(&repo, &["operation", "pending"]));
            if let Some(intent) = pending.lines().last() {
                let id = intent.split(' ').next().unwrap();
                ok(alt(&repo, &["operation", "recover", id]));
            }
            assert_eq!(ok(alt(&repo, &["operation", "pending"])), "", "point {at}");
            let now = (spelled(&repo), index(&repo));
            assert!(
                now == at_to || now == at_from,
                "{from} to {to}, point {at}: {now:?}"
            );
            finished += usize::from(now == at_to);
            clean(&repo, false);
            ok(alt(&repo, &["switch", from]));
            let back = (spelled(&repo), index(&repo));
            assert_eq!(back, at_from, "{from} to {to}, point {at}");
        }
        assert!(finished > 0 && finished < points, "{finished} of {points}");
    }
}
