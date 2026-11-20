//! `merge` and the executable bit: a file both sides changed keeps the bit
//! in the working tree where the merge stops, a side that only changed the
//! bit does not conflict with one that changed the content, and what is
//! added and committed afterwards has the mode git gives it.

use std::os::unix::fs::PermissionsExt;

use crate::byte_names::entries;
use crate::cwd_paths::{Trees, git};
use crate::scan_daemon::output;

const TREES: [&str; 3] = ["direct", "served", "git"];

/// Each file: whether it starts executable, then what the side branch and
/// the main branch do to it.
const FILES: &[(&str, bool, &[Do], &[Do])] = &[
    // the same line changed on both sides of an executable file
    ("both.sh", true, &[Middle("side")], &[Middle("main")]),
    // the bit on one side, the content on the other
    ("drop.sh", true, &[Bit(false)], &[Last("main")]),
    ("gain.sh", false, &[Bit(true)], &[Last("main")]),
    ("ours.sh", false, &[Last("side")], &[Bit(true)]),
    // the bit on both sides
    ("same.sh", false, &[Bit(true)], &[Bit(true)]),
    // the bit and one line on one side, another line on the other
    (
        "apart.sh",
        true,
        &[Bit(false), First("side")],
        &[Last("main")],
    ),
    // the bit and a line on one side, the same line on the other
    (
        "clash.sh",
        false,
        &[Bit(true), Middle("side")],
        &[Middle("main")],
    ),
    (
        "mine.sh",
        false,
        &[Middle("side")],
        &[Bit(true), Middle("main")],
    ),
];

const CONFLICTED: [&str; 3] = ["both.sh", "clash.sh", "mine.sh"];

#[derive(Clone, Copy)]
enum Do {
    Bit(bool),
    First(&'static str),
    Middle(&'static str),
    Last(&'static str),
}
use Do::*;

fn every(trees: &Trees, args: &[&str]) -> [Option<i32>; 3] {
    [
        output(&trees.at("direct"), args, false).status.code(),
        output(&trees.at("served"), args, true).status.code(),
        git(&trees.at("git"), args).status.code(),
    ]
}

fn executable(trees: &Trees, file: &str, on: bool) {
    for tree in TREES {
        let mode = if on { 0o755 } else { 0o644 };
        let permissions = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(trees.at(tree).join(file), permissions).unwrap();
    }
}

fn apply(trees: &Trees, file: &str, changes: &[Do]) {
    let mut lines = ["first", "second", "third", "fourth", "fifth"];
    for change in changes {
        match *change {
            Bit(on) => executable(trees, file, on),
            First(text) => lines[0] = text,
            Middle(text) => lines[2] = text,
            Last(text) => lines[4] = text,
        }
    }
    if lines != ["first", "second", "third", "fourth", "fifth"] {
        // written in place, which leaves the mode of the file as it is
        trees.write(file, &(lines.join("\n") + "\n"));
    }
}

fn commit(trees: &Trees, message: &str) {
    assert_eq!(every(trees, &["add", "."]), [Some(0); 3]);
    assert_eq!(every(trees, &["commit", "-m", message]), [Some(0); 3]);
}

/// Which of the files are executable in the working tree of `tree`.
fn bits(trees: &Trees, tree: &str) -> Vec<(&'static str, bool)> {
    let bit = |file: &str| {
        let meta = std::fs::metadata(trees.at(tree).join(file)).unwrap();
        meta.permissions().mode() & 0o100 != 0
    };
    FILES.iter().map(|(file, ..)| (*file, bit(file))).collect()
}

fn alike(trees: &Trees, when: &str) {
    let theirs = entries(trees, &trees.at("git").join(".git/index"));
    for tree in ["direct", "served"] {
        let ours = entries(trees, &trees.at(tree).join(".alt/index"));
        assert_eq!(
            String::from_utf8_lossy(&ours),
            String::from_utf8_lossy(&theirs),
            "the index of {tree} {when}"
        );
        assert_eq!(
            bits(trees, tree),
            bits(trees, "git"),
            "the executable files of {tree} {when}"
        );
    }
}

#[test]
fn merge_keeps_and_merges_the_executable_bit_as_git_does() {
    let trees = Trees::new();
    for (file, on, ..) in FILES {
        trees.write(file, "first\nsecond\nthird\nfourth\nfifth\n");
        executable(&trees, file, *on);
    }
    commit(&trees, "files");
    assert_eq!(every(&trees, &["switch", "-c", "side"]), [Some(0); 3]);
    for (file, _, side, _) in FILES {
        apply(&trees, file, side);
    }
    commit(&trees, "side");
    assert_eq!(every(&trees, &["switch", "main"]), [Some(0); 3]);
    for (file, _, _, main) in FILES {
        apply(&trees, file, main);
    }
    commit(&trees, "main");

    assert_eq!(every(&trees, &["merge", "side"]), [Some(1); 3]);
    alike(&trees, "where the merge stopped");

    // the markers of a conflict name the sides as each program does
    for file in CONFLICTED {
        trees.write(file, "resolved\n");
    }
    assert_eq!(every(&trees, &["add", "."]), [Some(0); 3]);
    alike(&trees, "with the conflicts resolved");
    assert_eq!(every(&trees, &["commit", "-m", "merged"]), [Some(0); 3]);
    alike(&trees, "after the commit");
    let tree = |tree: &str| {
        let at = trees.at(tree);
        match tree {
            "git" => git(&at, &["cat-file", "-p", "HEAD^{tree}"]).stdout,
            _ => {
                let commit = output(&at, &["cat-file", "-p", "HEAD"], false).stdout;
                let id = String::from_utf8(commit[5..45].to_vec()).unwrap();
                output(&at, &["cat-file", "-p", &id], false).stdout
            }
        }
    };
    for ours in ["direct", "served"] {
        assert_eq!(
            String::from_utf8_lossy(&tree(ours)),
            String::from_utf8_lossy(&tree("git")),
            "the tree {ours} committed"
        );
    }
}
