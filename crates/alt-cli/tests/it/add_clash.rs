//! A path is a file or a directory, never both: where `add` stages a file
//! beneath what the index has as a file, or a file where the index has a
//! directory, the entries that cannot stay go, as they do for git. What is
//! committed then is a tree git finds nothing wrong with.

use std::path::Path;

use crate::byte_names::{each, entries};
use crate::cwd_paths::{Trees, git};
use crate::scan_daemon::{output, run};

/// Where each case runs beneath its own directory, and the paths it adds.
const TO_DIRECTORY: &[(&str, &str)] = &[
    ("sub", "f.txt/x"),
    ("sub", "f.txt/"),
    ("sub", "f.txt/*"),
    ("sub", "*/x"),
    ("sub/f.txt", "."),
    ("", "sub/f.txt/x"),
];
const TO_FILE: &[(&str, &str)] = &[
    ("sub", "dee[p]"),
    ("sub", ":(glob)dee?"),
    ("sub", "deep"),
    ("", "sub/deep"),
];

fn cases() -> impl Iterator<Item = (usize, bool, &'static str, &'static str)> {
    let to_directory = TO_DIRECTORY.iter().map(|(at, path)| (true, *at, *path));
    let to_file = TO_FILE.iter().map(|(at, path)| (false, *at, *path));
    let all = to_directory.chain(to_file).enumerate();
    all.map(|(n, (kind, at, path))| (n, kind, at, path))
}

fn adds_alike(trees: &Trees, at: &str, path: &str) {
    let args = ["add", path];
    let direct = output(&trees.at("direct").join(at), &args, false);
    let served = output(&trees.at("served").join(at), &args, true);
    let by_git = git(&trees.at("git").join(at), &args);
    for (who, ours) in [("direct", &direct), ("served", &served)] {
        assert_eq!(ours.status.code(), by_git.status.code(), "{who}: {ours:?}");
    }
    let theirs = entries(trees, &trees.at("git").join(".git/index"));
    for tree in ["direct", "served"] {
        let ours = entries(trees, &trees.at(tree).join(".alt/index"));
        assert_eq!(ours, theirs, "the index of {tree} after add {path} in {at}");
    }
}

/// The tree of the commit `rev` names, as `cat-file` prints it.
fn tree_of(printed: &[u8]) -> Vec<u8> {
    printed.split(|b| *b == b'\n').next().unwrap().to_vec()
}

fn checked_by_git(exported: &Path) {
    let fsck = git(exported, &["fsck", "--strict"]);
    assert!(fsck.status.success(), "{fsck:?}");
    assert!(
        !fsck.stderr.windows(9).any(|w| w == b"duplicate"),
        "{fsck:?}"
    );
}

#[test]
fn add_leaves_no_path_a_file_and_a_directory_at_once() {
    let trees = Trees::new();
    for (n, ..) in cases() {
        trees.write(&format!("c{n}/sub/f.txt"), "f\n");
        trees.write(&format!("c{n}/sub/deep/g.txt"), "g\n");
        trees.write(&format!("c{n}/sub/deep/h.md"), "h\n");
    }
    each(&trees, &["add", "."]);
    each(&trees, &["commit", "-m", "cases"]);
    for (n, to_directory, at, path) in cases() {
        for tree in ["direct", "served", "git"] {
            let sub = trees.at(tree).join(format!("c{n}/sub"));
            match to_directory {
                true => {
                    std::fs::remove_file(sub.join("f.txt")).unwrap();
                    std::fs::create_dir(sub.join("f.txt")).unwrap();
                    std::fs::write(sub.join("f.txt/x"), "x\n").unwrap();
                }
                false => {
                    std::fs::remove_dir_all(sub.join("deep")).unwrap();
                    std::fs::write(sub.join("deep"), "file\n").unwrap();
                }
            }
        }
        adds_alike(&trees, &format!("c{n}/{at}"), path);
    }
    each(&trees, &["commit", "-m", "changed"]);
    let theirs = git(&trees.at("git"), &["cat-file", "-p", "HEAD"]);
    for (tree, daemon) in [("direct", false), ("served", true)] {
        let ours = output(&trees.at(tree), &["cat-file", "-p", "main"], daemon);
        assert_eq!(tree_of(&ours.stdout), tree_of(&theirs.stdout), "{tree}");
        let exported = trees.at(&format!("exported-{tree}"));
        run(
            &trees.at(tree),
            &["export", exported.to_str().unwrap()],
            false,
        );
        checked_by_git(&exported);
    }
}

/// Names that sort between a path and what lies beneath it (`a-b` and
/// `a.txt` between `a` and `a/x`) do not hide the entries beneath from the
/// file that takes the directory's place.
#[test]
fn a_file_displaces_what_lay_beneath_it_past_the_names_sorted_between() {
    let trees = Trees::new();
    for file in ["n/a/x", "n/a/deep/y", "n/a-b", "n/a.txt", "n/a0"] {
        trees.write(file, "tracked\n");
    }
    each(&trees, &["add", "."]);
    each(&trees, &["commit", "-m", "names"]);
    for tree in ["direct", "served", "git"] {
        std::fs::remove_dir_all(trees.at(tree).join("n/a")).unwrap();
        std::fs::write(trees.at(tree).join("n/a"), "file\n").unwrap();
    }
    // a pattern names the file alone: what lay beneath is not named gone
    adds_alike(&trees, "n", "[a]");
    each(&trees, &["commit", "-m", "a file"]);
}
