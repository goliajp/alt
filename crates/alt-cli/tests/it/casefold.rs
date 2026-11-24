//! On a file system that takes names differing in case for one, the
//! commands that write do what git does there with `core.ignoreCase`,
//! and none of them loses a file to another spelling of its name.
//!
//! These tests need such a file system: `ALT_CASEFOLD_DIR` names a
//! directory on one. They do not mount it themselves.

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::like_git::{Pair, alt, alt_daemon, fails, files, git, heads, index, ok};

fn place() -> PathBuf {
    let named = std::env::var_os("ALT_CASEFOLD_DIR")
        .expect("ALT_CASEFOLD_DIR names a directory on a file system that ignores case");
    let place = PathBuf::from(named);
    let probe = alt_os::temp::tempdir_in(&place).unwrap();
    std::fs::write(probe.path().join("spelled"), "").unwrap();
    assert!(
        probe.path().join("SPELLED").exists(),
        "{} is on a file system that tells case apart",
        place.display()
    );
    place
}

/// A pair on the file system that ignores case, with `a` and `d/x`
/// committed.
fn pair() -> Pair {
    let pair = Pair::new_in(&place());
    pair.write("a", "1\n");
    pair.write("d/x", "2\n");
    pair.commit_all("one");
    pair
}

/// The files of a tree with their names in lower case: which spelling a
/// directory shows for a file is the file system's to say.
fn folded(root: &Path) -> Vec<(String, String, bool)> {
    let lower =
        |(name, content, exec): (String, String, bool)| (name.to_lowercase(), content, exec);
    let mut found: Vec<_> = files(root).into_iter().map(lower).collect();
    found.sort();
    found
}

fn same_state(pair: &Pair) {
    assert_eq!(index(&pair.alt), ok(git(&pair.git, &["ls-files", "-s"])));
    assert_eq!(folded(&pair.alt), folded(&pair.git));
    assert_eq!(heads(&pair.alt), heads(&pair.git));
}

fn clean(repo: &Path, served: bool) {
    let status = match served {
        true => alt_daemon(repo, &["status"]),
        false => alt(repo, &["status"]),
    };
    let said = ok(status);
    assert!(
        said.contains("nothing to commit, working tree clean"),
        "{said}"
    );
}

/// Nothing in the tree differs from the index, and nothing is untracked.
fn clean_but_staged(repo: &Path) {
    let said = ok(alt(repo, &["status"]));
    assert!(
        !said.contains("not staged") && !said.contains("Untracked"),
        "{said}"
    );
}

fn code(o: &Output) -> Option<i32> {
    o.status.code()
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_new_store_is_told_that_its_file_system_ignores_case() {
    let pair = Pair::new_in(&place());
    let set = ok(git(&pair.git, &["config", "core.ignorecase"]));
    assert_eq!(set, "true\n");
    assert_eq!(ok(alt(&pair.alt, &["config", "core.ignorecase"])), set);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_file_takes_another_spelling_of_its_name_and_stays() {
    let pair = pair();
    pair.both(&["mv", "a", "A"]);
    same_state(&pair);
    assert_eq!(std::fs::read_to_string(pair.alt.join("A")).unwrap(), "1\n");
    // back under the old spelling by an undo, the file as it was
    ok(alt(&pair.alt, &["undo"]));
    assert!(index(&pair.alt).contains("\ta\n"));
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "1\n");
    clean(&pair.alt, false);
    ok(alt(&pair.alt, &["mv", "a", "A"]));
    pair.commit_all("respelled");
    same_state(&pair);
    // the file is the one the index names, whichever spelling the
    // directory shows for it
    clean(&pair.alt, false);
    clean(&pair.alt, true);
    pair.write("a", "changed\n");
    let said = ok(alt(&pair.alt, &["status"]));
    assert!(
        said.contains("modified:   A") && !said.contains("Untracked"),
        "{said}"
    );
    let said = ok(alt_daemon(&pair.alt, &["status"]));
    assert!(
        said.contains("modified:   A") && !said.contains("Untracked"),
        "{said}"
    );
    pair.commit_all("changed");
    same_state(&pair);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_directory_takes_another_spelling_of_its_name_with_what_it_holds() {
    let pair = pair();
    // git takes the other spelling for the directory to move into where
    // the file system refuses that, and ends there
    let (ours, theirs) = pair.run(&["mv", "d", "D"]);
    assert_eq!(
        code(&ours),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ours.stderr)
    );
    match theirs.status.success() {
        true => same_state(&pair),
        false => assert!(index(&pair.alt).contains("\tD/x\n")),
    }
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("D/x")).unwrap(),
        "2\n"
    );
    clean_but_staged(&pair.alt);
    ok(alt(&pair.alt, &["undo"]));
    assert!(index(&pair.alt).contains("\td/x\n"));
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("d/x")).unwrap(),
        "2\n"
    );
    clean(&pair.alt, false);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_switch_between_two_spellings_of_a_name_keeps_the_file() {
    let pair = pair();
    pair.both(&["switch", "-c", "up"]);
    pair.both(&["mv", "a", "A"]);
    pair.commit_all("respelled");
    pair.both(&["switch", "-c", "other"]);
    pair.write("A", "other\n");
    pair.commit_all("other content");
    for branch in ["main", "up", "main", "other", "up", "other", "main"] {
        pair.both(&["switch", branch]);
        same_state(&pair);
        clean(&pair.alt, false);
    }
    pair.both(&["switch", "other"]);
    ok(alt(&pair.alt, &["undo"]));
    pair.both(&["switch", "main"]);
    same_state(&pair);
    clean(&pair.alt, false);
    // a merge that only respells the name
    pair.write("z", "z\n");
    pair.commit_all("apart");
    ok(alt(&pair.alt, &["merge", "up"]));
    ok(git(&pair.git, &["merge", "-q", "--no-edit", "up"]));
    assert_eq!(index(&pair.alt), ok(git(&pair.git, &["ls-files", "-s"])));
    assert_eq!(folded(&pair.alt), folded(&pair.git));
    clean(&pair.alt, false);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_file_in_the_way_under_another_spelling_is_left_as_it_is() {
    let pair = pair();
    pair.both(&["switch", "-c", "up"]);
    pair.both(&["mv", "a", "A"]);
    pair.commit_all("respelled");
    pair.both(&["switch", "main"]);
    // a change that is not committed
    pair.write("a", "mine\n");
    let (ours, theirs) = pair.run(&["switch", "up"]);
    assert_eq!((code(&ours), code(&theirs)), (Some(1), Some(1)));
    same_state(&pair);
    pair.both(&["restore", "a"]);
    // a file that is not tracked
    pair.both(&["rm", "-q", "a"]);
    pair.commit_all("gone");
    pair.write("a", "mine\n");
    pair.same_failure(&["switch", "up"], 1);
    same_state(&pair);
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("a")).unwrap(),
        "mine\n"
    );
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_path_is_taken_as_it_is_spelled_by_the_commands_that_name_one() {
    let pair = pair();
    pair.write("new", "n\n");
    pair.both(&["add", "new", "NEW"]);
    same_state(&pair);
    pair.write("a", "m\n");
    pair.both(&["add", "A"]);
    same_state(&pair);
    pair.same_failure(&["checkout", "--", "A"], 1);
    pair.same_failure(&["restore", "A"], 1);
    pair.same_failure(&["rm", "A"], 128);
    pair.both(&["reset", "-q", "--", "A"]);
    same_state(&pair);
    assert_eq!(std::fs::read_to_string(pair.alt.join("a")).unwrap(), "m\n");
    // a file put away untracked does not come back over another spelling
    pair.both(&["add", "a"]);
    pair.commit_all("two");
    pair.write("loose", "u\n");
    pair.both(&["stash", "-u", "-q"]);
    pair.write("LOOSE", "other\n");
    let (ours, theirs) = pair.run(&["stash", "pop"]);
    assert!(!ours.status.success() && !theirs.status.success());
    assert_eq!(folded(&pair.alt), folded(&pair.git));
    assert_eq!(
        std::fs::read_to_string(pair.alt.join("LOOSE")).unwrap(),
        "other\n"
    );
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn two_paths_that_are_one_file_are_not_written_over_each_other() {
    // made where case is told apart: `both` holds `a` and `A`
    let made = alt_os::temp::tempdir().unwrap();
    let source = made.path();
    ok(git(source, &["init", "-q", "."]));
    std::fs::write(source.join("a"), "1\n").unwrap();
    ok(git(source, &["add", "."]));
    ok(git(source, &["commit", "-q", "-m", "one"]));
    ok(git(source, &["switch", "-q", "-c", "both"]));
    std::fs::write(source.join("A"), "other\n").unwrap();
    ok(git(source, &["add", "."]));
    ok(git(source, &["commit", "-q", "-m", "both"]));
    ok(git(source, &["switch", "-q", "main"]));
    let held = alt_os::temp::tempdir_in(place()).unwrap();
    let repo = held.path().join("r");
    ok(alt(source, &["import", repo.to_str().unwrap()]));
    for refused in [
        &["switch", "both"][..],
        &["merge", "both"],
        &["cherry-pick", "both"],
    ] {
        let o = alt(&repo, refused);
        let said = fails(o, if refused[0] == "switch" { 1 } else { 75 });
        assert!(said.contains("would be overwritten"), "{said}");
        assert_eq!(std::fs::read_to_string(repo.join("a")).unwrap(), "1\n");
        clean(&repo, false);
    }
}
