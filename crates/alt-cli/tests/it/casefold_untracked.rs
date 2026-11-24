//! A directory that holds a file nothing tracks or ignores, in the way
//! of a file that has the directory's name to the file system alone:
//! in another case, or with the same letters composed otherwise. The
//! switch is refused, as git refuses it, and the file stays.

use std::path::Path;

use crate::casefold::{place, same_state};
use crate::like_git::{Pair, alt, fails, git, ok};

/// The names at the top of the tree at `root`, as the directory spells
/// them.
pub(crate) fn spelled(root: &Path) -> Vec<String> {
    let names = std::fs::read_dir(root).unwrap();
    let names = names.map(|entry| entry.unwrap().file_name().into_string().unwrap());
    let mut names: Vec<String> = names.filter(|n| n != ".alt" && n != ".git").collect();
    names.sort();
    names
}

/// The switch to `branch`, which has a file at `file`, is refused over
/// the file at `mine`; once that file is gone it is made.
fn refused_then_made(pair: &Pair, branch: &str, file: &str, mine: &str, names: [&[&str]; 2]) {
    let ours = alt(&pair.alt, &["switch", branch]);
    let said = String::from_utf8_lossy(&ours.stderr).into_owned();
    assert!(!ours.status.success(), "{said}");
    let why = format!("checkout target '{file}' is a directory holding untracked files");
    assert!(said.contains(&why), "{said}");
    let theirs = fails(git(&pair.git, &["switch", branch]), 1);
    let why = format!("would lose untracked files in them:\n\t{file}\n");
    assert!(theirs.contains(&why), "{theirs}");
    for root in [&pair.alt, &pair.git] {
        assert_eq!(std::fs::read_to_string(root.join(mine)).unwrap(), "mine\n");
        assert_eq!(spelled(root), names[0]);
    }
    assert!(ok(alt(&pair.alt, &["status"])).contains("On branch main"));
    assert_eq!(ok(alt(&pair.alt, &["operation", "pending"])), "");
    same_state(pair);
    pair.remove(mine);
    pair.both(&["switch", branch]);
    for root in [&pair.alt, &pair.git] {
        assert_eq!(spelled(root), names[1]);
    }
    same_state(pair);
}

/// `main` with `keep`, and `branch` with the file `file` beside it; on
/// `main`.
fn a_file_on_a_branch(branch: &str, file: &str) -> Pair {
    let pair = Pair::new_in(&place());
    pair.write("keep", "k\n");
    pair.commit_all("base");
    pair.both(&["switch", "-c", branch]);
    pair.write(file, "a file\n");
    pair.commit_all("a file");
    pair.both(&["switch", "main"]);
    pair
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn an_untracked_file_in_a_directory_of_another_case_stops_a_switch_to_a_file() {
    let pair = a_file_on_a_branch("file", "A");
    pair.write("a/y", "mine\n");
    refused_then_made(&pair, "file", "A", "a/y", [&["a", "keep"], &["A", "keep"]]);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn an_untracked_file_in_a_directory_composed_otherwise_stops_a_switch_to_a_file() {
    // one code point on the branch, a letter and a combining accent here
    let (composed, apart) = ("\u{e9}", "e\u{301}");
    let probe = alt_os::temp::tempdir_in(place()).unwrap();
    std::fs::write(probe.path().join(composed), "").unwrap();
    let one = probe.path().join(apart).exists();
    assert!(one, "the file system tells the two compositions apart");
    let pair = a_file_on_a_branch("file", composed);
    let mine = format!("{apart}/y");
    pair.write(&mine, "mine\n");
    let names: [&[&str]; 2] = [&[apart, "keep"], &["keep", composed]];
    refused_then_made(&pair, "file", composed, &mine, names);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn an_untracked_file_beside_a_tracked_one_stops_a_switch_that_makes_their_directory_a_file() {
    let pair = Pair::new_in(&place());
    pair.write("keep", "k\n");
    pair.write("D/y", "y\n");
    pair.commit_all("base");
    pair.both(&["switch", "-c", "flat"]);
    pair.both(&["rm", "D/y"]);
    pair.write("d", "a file\n");
    pair.commit_all("flat");
    pair.both(&["switch", "main"]);
    pair.write("D/u", "mine\n");
    refused_then_made(&pair, "flat", "d", "D/u", [&["D", "keep"], &["d", "keep"]]);
}
