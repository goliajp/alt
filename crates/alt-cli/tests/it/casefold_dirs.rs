//! Directories whose names differ in case alone, names that fold beyond
//! the case of a letter, and a respelling cut short, on a file system
//! that takes names differing in case for one.

use std::path::Path;

use crate::casefold::{clean, clean_but_staged, code, pair, place};
use crate::casefold_wider::cut;
use crate::like_git::{Pair, alt, alt_daemon, git, index, ok};

pub(crate) fn read(root: &Path, file: &str) -> String {
    std::fs::read_to_string(root.join(file)).unwrap()
}

/// What `alt status` tells of the tree, without the lines that say how
/// to go on.
pub(crate) fn told(said: &str) -> Vec<String> {
    let mut lines: Vec<String> = said
        .lines()
        .filter(|line| line.starts_with('\t'))
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    lines.sort();
    lines
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn two_directories_that_are_one_to_the_file_system_are_walked_as_one() {
    // made where case is told apart
    let made = alt_os::temp::tempdir().unwrap();
    let source = made.path();
    ok(git(source, &["init", "-q", "."]));
    for file in ["d/x", "D/y", "d/sub/s", "D/sub/t", "D/only/o"] {
        let path = source.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("{file}\n")).unwrap();
    }
    ok(git(source, &["add", "."]));
    ok(git(source, &["commit", "-q", "-m", "both"]));
    let held = alt_os::temp::tempdir_in(place()).unwrap();
    let (repo, cloned) = (held.path().join("r"), held.path().join("g"));
    let said = ok(alt(source, &["import", repo.to_str().unwrap()]));
    assert!(!said.contains("collided"), "{said}");
    let clone = git(source, &["clone", "-q", ".", cloned.to_str().unwrap()]);
    assert!(clone.status.success() && clone.stderr.is_empty());
    // no two paths are one file: nothing differs, as for git
    clean(&repo, false);
    assert_eq!(ok(git(&cloned, &["status", "--porcelain"])), "");
    for root in [&repo, &cloned] {
        std::fs::write(root.join("d/u"), "u\n").unwrap();
        std::fs::write(root.join("D/sub/u2"), "u2\n").unwrap();
        std::fs::write(root.join("D/x"), "changed\n").unwrap();
        std::fs::remove_file(root.join("d/sub/t")).unwrap();
    }
    let theirs = ok(git(&cloned, &["status", "--porcelain"]));
    assert_eq!(theirs, " D D/sub/t\n M d/x\n?? D/sub/u2\n?? D/u\n");
    let ours = told(&ok(alt(&repo, &["status"])));
    assert_eq!(
        ours,
        ["D/sub/u2", "D/u", "deleted: D/sub/t", "modified: d/x"],
    );
    // the daemon, which keeps what it saw of the directories, tells the same
    assert_eq!(told(&ok(alt_daemon(&repo, &["status"]))), ours);
    std::fs::write(repo.join("D/only/new"), "n\n").unwrap();
    std::fs::write(cloned.join("D/only/new"), "n\n").unwrap();
    let served = told(&ok(alt_daemon(&repo, &["status"])));
    assert_eq!(served, told(&ok(alt(&repo, &["status"]))));
    assert!(served.contains(&"D/only/new".to_owned()), "{served:?}");
    ok(alt(&repo, &["add", "-A"]));
    ok(git(&cloned, &["add", "-A"]));
    assert_eq!(index(&repo), ok(git(&cloned, &["ls-files", "-s"])));
    ok(alt(&repo, &["commit", "-m", "worked in"]));
    clean(&repo, false);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_name_takes_a_spelling_of_another_length() {
    // one letter for two, and a letter with three cases
    for (from, to) in [("straße", "STRASSE"), ("ǆ", "ǅ")] {
        let pair = Pair::new_in(&place());
        pair.write(from, "e\n");
        pair.commit_all("one");
        assert!(pair.alt.join(to).exists(), "{from} and {to} are two names");
        let (ours, theirs) = pair.run(&["mv", from, to]);
        let said = String::from_utf8_lossy(&ours.stderr).into_owned();
        assert_eq!(code(&ours), Some(0), "{said}");
        assert_eq!(code(&theirs), Some(128));
        assert_eq!(read(&pair.alt, to), "e\n");
        clean_but_staged(&pair.alt);
        ok(alt(&pair.alt, &["commit", "-m", "respelled"]));
        clean(&pair.alt, false);
        let listed = ok(alt(&pair.alt, &["rev-parse", &format!("HEAD:{to}")]));
        assert_eq!(
            listed,
            ok(alt(&pair.alt, &["rev-parse", &format!("HEAD~1:{from}")]))
        );
        // written under the spelling it had, it is the tracked file changed
        std::fs::write(pair.alt.join(from), "changed\n").unwrap();
        let said = ok(alt(&pair.alt, &["status"]));
        let lines = told(&said);
        assert!(
            lines.len() == 1 && lines[0].starts_with("modified: "),
            "{said}"
        );
        ok(alt(&pair.alt, &["restore", to]));
        assert_eq!(read(&pair.alt, from), "e\n");
        ok(alt(&pair.alt, &["undo"]));
        ok(alt(&pair.alt, &["undo"]));
        clean(&pair.alt, false);
    }
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_respelling_cut_short_leaves_the_file_tracked_and_is_made_again() {
    for (from, to, file, path) in [("a", "A", "a", "A"), ("d", "D", "d/x", "D/x")] {
        let pair = pair();
        let repo = &pair.alt;
        let before = index(repo);
        let list = repo.join(".alt/points");
        let whole = cut(repo, &["mv", from, to], &format!("list:{}", list.display()));
        assert_eq!(code(&whole), Some(0));
        let after = index(repo);
        assert!(after.contains(&format!("\t{path}\n")), "{after}");
        let points = std::fs::read_to_string(&list).unwrap().lines().count();
        std::fs::remove_file(&list).unwrap();
        assert!(points > 2, "{points}");
        ok(alt(repo, &["mv", to, from]));
        assert_eq!(index(repo), before);
        for at in 1..=points {
            let killed = cut(repo, &["mv", from, to], &at.to_string());
            assert_eq!(code(&killed), None, "point {at}");
            let pending = ok(alt(repo, &["operation", "pending"]));
            if let Some(intent) = pending.lines().last() {
                let id = intent.split(' ').next().unwrap();
                ok(alt(repo, &["operation", "recover", id]));
            }
            assert_eq!(ok(alt(repo, &["operation", "pending"])), "", "point {at}");
            // the file is there and tracked, under one spelling or the other
            assert_eq!(read(repo, file), read(repo, path), "point {at}");
            clean_but_staged(repo);
            let now = index(repo);
            assert!(now == before || now == after, "point {at}: {now}");
            if now == before {
                ok(alt(repo, &["mv", from, to]));
            }
            assert_eq!(index(repo), after, "point {at}");
            ok(alt(repo, &["mv", to, from]));
            assert_eq!(index(repo), before, "point {at}");
            clean(repo, false);
        }
    }
}
