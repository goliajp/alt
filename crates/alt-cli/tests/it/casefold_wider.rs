//! More of what the commands do on a file system that takes names
//! differing in case for one: across two spellings of a name, served by
//! the daemon, in a parallel workspace, cut short, and with names that
//! differ in more than the case of a letter of the alphabet.

use std::path::Path;
use std::process::Output;

use crate::casefold::{clean, clean_but_staged, code, folded, pair, place, same_state};
use crate::like_git::{Pair, alt, alt_daemon, fails, git, index, ok};

fn read(root: &Path, file: &str) -> String {
    std::fs::read_to_string(root.join(file)).unwrap()
}

/// `main` has `a`; `up` has the file as `A`, with other content.
fn two_spellings() -> Pair {
    let pair = pair();
    pair.both(&["switch", "-c", "up"]);
    pair.both(&["mv", "a", "A"]);
    pair.write("A", "up\n");
    pair.commit_all("respelled");
    pair.both(&["switch", "main"]);
    pair
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_reset_a_checkout_of_a_path_and_a_stash_cross_two_spellings() {
    let pair = two_spellings();
    pair.both(&["tag", "first"]);
    pair.both(&["reset", "-q", "--hard", "up"]);
    same_state(&pair);
    assert_eq!(read(&pair.alt, "A"), "up\n");
    clean(&pair.alt, false);
    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(read(&pair.alt, "a"), "1\n");
    clean(&pair.alt, false);
    ok(alt(&pair.alt, &["reset", "-q", "--hard", "up"]));
    pair.both(&["reset", "-q", "--hard", "first"]);
    same_state(&pair);
    clean(&pair.alt, false);
    // the path of a commit beside the path the index has for the file
    pair.both(&["checkout", "up", "--", "A"]);
    same_state(&pair);
    assert_eq!(read(&pair.alt, "a"), "up\n");
    pair.both(&["reset", "-q", "--hard"]);
    same_state(&pair);
    assert_eq!(read(&pair.alt, "a"), "1\n");
    // a change put away under one spelling does not come back over the
    // file the other spelling tracks
    pair.write("a", "mine\n");
    pair.both(&["stash", "-q"]);
    pair.both(&["switch", "up"]);
    let (ours, theirs) = pair.run(&["stash", "pop"]);
    assert_eq!((code(&ours), code(&theirs)), (Some(1), Some(1)));
    let said = String::from_utf8_lossy(&ours.stderr).into_owned();
    assert!(said.contains("'a' and 'A' are one file here"), "{said}");
    assert_eq!(read(&pair.alt, "A"), "up\n");
    clean(&pair.alt, false);
    assert_eq!(ok(alt(&pair.alt, &["stash", "list"])).lines().count(), 1);
    ok(alt(&pair.alt, &["switch", "main"]));
    ok(alt(&pair.alt, &["stash", "pop", "-q"]));
    assert_eq!(read(&pair.alt, "a"), "mine\n");
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn the_daemon_and_a_parallel_workspace_respell_and_switch_alike() {
    let pair = pair();
    pair.both(&["switch", "-c", "up"]);
    ok(alt_daemon(&pair.alt, &["mv", "a", "A"]));
    ok(git(&pair.git, &["mv", "a", "A"]));
    same_state(&pair);
    assert_eq!(read(&pair.alt, "A"), "1\n");
    pair.write("A", "up\n");
    pair.commit_all("respelled");
    for branch in ["main", "up", "main"] {
        ok(alt_daemon(&pair.alt, &["switch", branch]));
        ok(git(&pair.git, &["switch", "-q", branch]));
        same_state(&pair);
        clean(&pair.alt, true);
    }
    // a workspace of its own tree, on the same file system
    let held = alt_os::temp::tempdir_in(place()).unwrap();
    let tree = held.path().join("tree");
    let tree_said = tree.to_str().unwrap();
    ok(alt(&pair.alt, &["branch", "third", "main"]));
    ok(alt(
        &pair.alt,
        &["workspace", "add", "ws", tree_said, "third"],
    ));
    let ws = |args: &[&str]| alt(&pair.alt, &[&["--workspace", "ws"], args].concat());
    ok(ws(&["mv", "a", "A"]));
    assert_eq!(read(&tree, "A"), "1\n");
    ok(ws(&["commit", "-m", "respelled here"]));
    let said = ok(ws(&["status"]));
    assert!(said.contains("working tree clean"), "{said}");
    ok(ws(&["switch", "-c", "fourth", "main"]));
    assert_eq!(read(&tree, "a"), "1\n");
    let said = ok(ws(&["status"]));
    assert!(said.contains("working tree clean"), "{said}");
    ok(ws(&["undo"]));
    assert_eq!(read(&tree, "A"), "1\n");
    // the tree of the store itself is as it was
    assert_eq!(read(&pair.alt, "a"), "1\n");
    clean(&pair.alt, false);
}

pub(crate) fn cut(repo: &Path, args: &[&str], crash: &str) -> Output {
    crate::alt_command()
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("ALT_CRASH_POINT", crash)
        .args(args)
        .output()
        .unwrap()
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_switch_between_two_spellings_cut_short_is_finished_with_the_file_in_place() {
    let pair = two_spellings();
    let repo = &pair.alt;
    let list = repo.join(".alt/points");
    let whole = cut(repo, &["switch", "up"], &format!("list:{}", list.display()));
    assert_eq!(code(&whole), Some(0));
    let points = std::fs::read_to_string(&list).unwrap().lines().count();
    std::fs::remove_file(&list).unwrap();
    assert!(points > 3, "{points}");
    ok(alt(repo, &["switch", "main"]));
    let (at_main, at_up) = (index(repo), {
        ok(alt(repo, &["switch", "up"]));
        let at_up = index(repo);
        ok(alt(repo, &["switch", "main"]));
        at_up
    });
    for at in 1..=points {
        let killed = cut(repo, &["switch", "up"], &at.to_string());
        assert_eq!(code(&killed), None, "point {at}");
        let pending = ok(alt(repo, &["operation", "pending"]));
        if let Some(intent) = pending.lines().last() {
            let id = intent.split(' ').next().unwrap();
            ok(alt(repo, &["operation", "recover", id]));
        }
        assert_eq!(ok(alt(repo, &["operation", "pending"])), "", "point {at}");
        // the switch was made whole or not at all, and the one file is
        // there with what the branch has for it
        let now = index(repo);
        let content = read(repo, "a");
        if now == at_up {
            assert_eq!(content, "up\n", "point {at}");
        } else {
            assert_eq!(
                (now, content),
                (at_main.clone(), "1\n".into()),
                "point {at}"
            );
        }
        clean(repo, false);
        ok(alt(repo, &["switch", "main"]));
        assert_eq!(read(repo, "a"), "1\n");
        clean(repo, false);
    }
    // undone, and the undo undone
    ok(alt(repo, &["switch", "up"]));
    ok(alt(repo, &["undo"]));
    assert_eq!((index(repo), read(repo, "a")), (at_main, "1\n".into()));
    clean(repo, false);
    ok(alt(repo, &["undo"]));
    assert_eq!((index(repo), read(repo, "a")), (at_up, "up\n".into()));
    clean(repo, false);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn an_import_of_two_paths_that_are_one_file_says_so_and_can_be_worked_in() {
    // made where case is told apart
    let made = alt_os::temp::tempdir().unwrap();
    let source = made.path();
    ok(git(source, &["init", "-q", "."]));
    for (file, content) in [("a", "1\n"), ("A", "other\n"), ("keep", "k\n")] {
        std::fs::write(source.join(file), content).unwrap();
    }
    ok(git(source, &["add", "."]));
    ok(git(source, &["commit", "-q", "-m", "both"]));
    let held = alt_os::temp::tempdir_in(place()).unwrap();
    let (repo, cloned) = (held.path().join("r"), held.path().join("g"));
    let said = ok(alt(source, &["import", repo.to_str().unwrap()]));
    let warned = "warning: the following paths have collided (e.g. case-sensitive paths\n\
                  on a case-insensitive filesystem) and only one from the same\n\
                  colliding group is in the working tree:\n\n  'A'\n  'a'\n";
    assert!(said.contains(warned), "{said}");
    let clone = git(source, &["clone", "-q", ".", cloned.to_str().unwrap()]);
    let theirs = String::from_utf8_lossy(&clone.stderr).into_owned();
    assert!(
        clone.status.success() && theirs.contains(warned),
        "{theirs}"
    );
    // one file for the two, with what one of them has; the other path is
    // told as changed, and neither as gone
    assert_eq!(folded(&repo), folded(&cloned));
    let status = ok(alt(&repo, &["status"]));
    assert_eq!(status.matches("modified:").count(), 1, "{status}");
    assert!(
        !status.contains("deleted:") && !status.contains("Untracked"),
        "{status}"
    );
    let theirs = ok(git(&cloned, &["status", "--porcelain"]));
    assert_eq!(theirs.lines().count(), 1, "{theirs}");
    // the store is whole, and work goes on in it
    assert_eq!(index(&repo), ok(git(&cloned, &["ls-files", "-s"])));
    std::fs::write(repo.join("keep"), "more\n").unwrap();
    ok(alt(&repo, &["add", "keep"]));
    ok(alt(&repo, &["commit", "-m", "more"]));
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_name_takes_a_spelling_that_differs_beyond_the_alphabet() {
    let pair = Pair::new_in(&place());
    pair.write("é", "e\n");
    pair.commit_all("one");
    // the file system says the two names are one file; git looks at the
    // letters of the alphabet alone and finds a file in the way
    let (ours, theirs) = pair.run(&["mv", "é", "É"]);
    assert_eq!(
        code(&ours),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ours.stderr)
    );
    assert_eq!(code(&theirs), Some(128));
    assert!(
        index(&pair.alt).contains("\\303\\211"),
        "{}",
        index(&pair.alt)
    );
    assert_eq!(read(&pair.alt, "É"), "e\n");
    clean_but_staged(&pair.alt);
    ok(alt(&pair.alt, &["undo"]));
    let back = index(&pair.alt);
    assert!(back.contains("\\303\\251"), "{back}");
    assert_eq!(read(&pair.alt, "é"), "e\n");
    clean(&pair.alt, false);
    ok(alt(&pair.alt, &["mv", "é", "É"]));
    ok(alt(&pair.alt, &["commit", "-m", "respelled"]));
    clean(&pair.alt, false);
    pair.write("é", "changed\n");
    let said = ok(alt(&pair.alt, &["status"]));
    assert!(
        said.contains("modified:") && !said.contains("Untracked") && !said.contains("deleted"),
        "{said}"
    );
    ok(alt(&pair.alt, &["restore", "É"]));
    assert_eq!(read(&pair.alt, "é"), "e\n");
    clean(&pair.alt, false);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_workspace_goes_by_the_file_system_its_own_tree_is_on() {
    // the store where case is told apart, a workspace of it where it is not
    let plain = Pair::new();
    plain.write("a", "1\n");
    plain.commit_all("one");
    assert!(!ok(alt(&plain.alt, &["config", "--list"])).contains("ignorecase"));
    let held = alt_os::temp::tempdir_in(place()).unwrap();
    let tree = held.path().join("tree");
    ok(alt(&plain.alt, &["branch", "there", "main"]));
    let add = ["workspace", "add", "ws", tree.to_str().unwrap(), "there"];
    ok(alt(&plain.alt, &add));
    let ws = |args: &[&str]| alt(&plain.alt, &[&["--workspace", "ws"], args].concat());
    ok(ws(&["mv", "a", "A"]));
    assert_eq!(read(&tree, "A"), "1\n");
    ok(ws(&["commit", "-m", "respelled"]));
    let said = ok(ws(&["status"]));
    assert!(said.contains("working tree clean"), "{said}");
    // the store where it is not told apart, a workspace where it is: two
    // spellings are two files there
    let folding = pair();
    let held = alt_os::temp::tempdir().unwrap();
    let tree = held.path().join("tree");
    ok(alt(&folding.alt, &["branch", "there", "main"]));
    let add = ["workspace", "add", "ws", tree.to_str().unwrap(), "there"];
    ok(alt(&folding.alt, &add));
    let ws = |args: &[&str]| alt(&folding.alt, &[&["--workspace", "ws"], args].concat());
    std::fs::write(tree.join("A"), "another\n").unwrap();
    let said = ok(ws(&["status"]));
    assert!(
        said.contains("Untracked") && said.contains("\tA\n") && !said.contains("modified"),
        "{said}"
    );
    assert_eq!(read(&tree, "a"), "1\n");
    ok(ws(&["add", "A"]));
    ok(ws(&["commit", "-m", "two files"]));
    // which the tree of the store cannot hold both of
    let said = fails(alt(&folding.alt, &["merge", "there"]), 75);
    assert!(said.contains("are one file here"), "{said}");
    assert_eq!(read(&folding.alt, "a"), "1\n");
    clean(&folding.alt, false);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_setting_that_denies_what_the_file_system_does_loses_no_file() {
    let pair = two_spellings();
    pair.both(&["config", "core.ignorecase", "false"]);
    // git goes by the setting: the other spelling is a file in the way
    let (ours, theirs) = pair.run(&["switch", "up"]);
    assert_eq!((code(&ours), code(&theirs)), (Some(0), Some(1)));
    assert_eq!(read(&pair.alt, "A"), "up\n");
    clean(&pair.alt, false);
    ok(alt(&pair.alt, &["switch", "main"]));
    assert_eq!(read(&pair.alt, "a"), "1\n");
    let (ours, theirs) = pair.run(&["mv", "a", "A"]);
    assert_eq!((code(&ours), code(&theirs)), (Some(0), Some(128)));
    assert_eq!(read(&pair.alt, "A"), "1\n");
    clean_but_staged(&pair.alt);
}
