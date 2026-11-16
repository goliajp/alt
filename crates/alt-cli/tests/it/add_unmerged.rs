//! `add` beside the stages a merge left: where a path with a conflict, or
//! one next to it, is a directory or a file now, the entries that go and
//! those that stay are the ones git drops and keeps.

use std::path::Path;

use crate::byte_names::entries;
use crate::cwd_paths::{Trees, git};
use crate::scan_daemon::output;

/// What becomes of the tree of a case once the merge has stopped.
#[derive(Clone, Copy)]
enum Then {
    Nothing,
    /// the conflicted `sub/f.txt` is a directory
    FileToDirectory,
    /// `d`, with a conflict in it, is a file
    DirectoryToFile,
    /// the conflicted `a` is gone
    Gone,
    /// the conflicted `a` is a directory
    ConflictToDirectory,
    /// `keep`, which no side changed, is a directory
    KeptToDirectory,
    /// `sub`, with a conflict in it, is a file
    SubToFile,
}
use Then::*;

/// What is done to the tree, where the `add` runs beneath the directory of
/// its case, and its arguments.
const CASES: &[(Then, &str, &[&str])] = &[
    (FileToDirectory, "", &["sub/f.txt/x"]),
    (FileToDirectory, "", &["."]),
    (FileToDirectory, "", &["sub/f.txt"]),
    (FileToDirectory, "", &["-u", "."]),
    (FileToDirectory, "", &["-A", "."]),
    (FileToDirectory, "sub", &["-u", "f.txt"]),
    (FileToDirectory, "sub", &["f.txt/*"]),
    (FileToDirectory, "", &["--renormalize", "."]),
    (DirectoryToFile, "", &["d"]),
    (DirectoryToFile, "", &["."]),
    (DirectoryToFile, "", &["-u", "."]),
    (DirectoryToFile, "", &["-u", "d"]),
    (DirectoryToFile, "", &["d*"]),
    (DirectoryToFile, "", &["d.txt"]),
    (Nothing, "", &["a"]),
    (Nothing, "", &["-u", "."]),
    (Nothing, "", &["a-b"]),
    (Nothing, "", &["keep"]),
    (Gone, "", &["-u", "."]),
    (Gone, "", &["."]),
    (Gone, "", &["a"]),
    (ConflictToDirectory, "", &["a/b"]),
    (ConflictToDirectory, "", &["a"]),
    (ConflictToDirectory, "", &["-u", "a"]),
    (KeptToDirectory, "", &["keep/k"]),
    (SubToFile, "", &["sub"]),
    (SubToFile, "", &["-u", "."]),
];

const CONFLICTED: [&str; 3] = ["a", "d/x", "sub/f.txt"];

fn every(trees: &Trees, args: &[&str]) {
    output(&trees.at("direct"), args, false);
    output(&trees.at("served"), args, true);
    git(&trees.at("git"), args);
}

/// A merge stopped by a conflict in three files of every case.
fn merged(trees: &Trees) {
    let write = |files: &[&str], body: &str| {
        for (n, _) in CASES.iter().enumerate() {
            for file in files {
                trees.write(&format!("c{n}/{file}"), body);
            }
        }
        every(trees, &["add", "."]);
        every(trees, &["commit", "-m", body]);
    };
    let beside = ["a-b", "a.txt", "d/y", "d.txt", "keep"];
    write(&[&CONFLICTED[..], &beside[..]].concat(), "base\n");
    every(trees, &["switch", "-c", "side"]);
    write(&CONFLICTED, "side\n");
    every(trees, &["switch", "main"]);
    write(&CONFLICTED, "main\n");
    every(trees, &["merge", "side"]);
}

fn change(case: &Path, then: Then) {
    let directory = |name: &str, file: &str| {
        std::fs::remove_file(case.join(name)).unwrap();
        std::fs::create_dir(case.join(name)).unwrap();
        std::fs::write(case.join(name).join(file), "new\n").unwrap();
    };
    let file = |name: &str| {
        std::fs::remove_dir_all(case.join(name)).unwrap();
        std::fs::write(case.join(name), "file\n").unwrap();
    };
    // the markers of a conflict name the sides as each program does
    for conflicted in CONFLICTED {
        std::fs::write(case.join(conflicted), "resolved\n").unwrap();
    }
    match then {
        Nothing => {}
        FileToDirectory => directory("sub/f.txt", "x"),
        DirectoryToFile => file("d"),
        Gone => std::fs::remove_file(case.join("a")).unwrap(),
        ConflictToDirectory => directory("a", "b"),
        KeptToDirectory => directory("keep", "k"),
        SubToFile => file("sub"),
    }
}

/// `add` with `args` ends as git's does in each tree and leaves the index
/// git leaves, stage by stage.
fn adds_alike(trees: &Trees, at: &str, args: &[&str]) {
    let args = [&["add"][..], args].concat();
    let by_git = git(&trees.at("git").join(at), &args);
    let theirs = entries(trees, &trees.at("git").join(".git/index"));
    for (tree, daemon) in [("direct", false), ("served", true)] {
        let ours = output(&trees.at(tree).join(at), &args, daemon);
        assert_eq!(ours.status.code(), by_git.status.code(), "{tree}: {ours:?}");
        let ours = entries(trees, &trees.at(tree).join(".alt/index"));
        assert_eq!(ours, theirs, "the index of {tree} after {args:?} in {at}");
    }
}

#[test]
fn add_beside_the_stages_of_a_conflict_keeps_and_drops_what_git_does() {
    let trees = Trees::new();
    merged(&trees);
    let theirs = entries(&trees, &trees.at("git").join(".git/index"));
    let ours = entries(&trees, &trees.at("direct").join(".alt/index"));
    assert_eq!(ours, theirs, "the stages of the merge");
    let stages = |index: &[u8]| {
        index
            .split(|b| *b == 0)
            .filter(|e| e.contains(&b'\t'))
            .count()
    };
    assert_eq!(
        stages(&theirs) - 4,
        CASES.len() * (CONFLICTED.len() * 3 + 5)
    );
    for (n, (then, at, args)) in CASES.iter().enumerate() {
        for tree in ["direct", "served", "git"] {
            change(&trees.at(tree).join(format!("c{n}")), *then);
        }
        adds_alike(&trees, &format!("c{n}/{at}"), args);
        adds_alike(&trees, &format!("c{n}"), &["."]);
    }
}
