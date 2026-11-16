//! `add` without a path stages nothing and says so, `-u` stages what
//! changed in the files the index has, `-A` every change, both in the
//! whole tree when no path is given. Every command here is run by git, by
//! `alt` and by its daemon, which end alike, say the same but for advice
//! and leave the same index.

use std::process::Output;

use alt_bytes::{BStr, ByteSlice};

use crate::byte_names::{each, entries};
use crate::cwd_paths::{Trees, git};
use crate::scan_daemon::{output, write};

const CASES: &[&[&str]] = &[
    &[],
    &["-f"],
    &["--"],
    &["--renormalize"],
    &["-u"],
    &["--update"],
    &["-A"],
    &["--all"],
    &["-u", "."],
    &["-u", "n.txt"],
    &["-u", "f.txt", "n.txt"],
    &["-u", "nosuch"],
    &["-u", "f.txt", "nosuch"],
    &["-u", "n*"],
    &["-u", "x.log"],
    &["-u", "*.txt"],
    &["-u", ":!f.txt"],
    &["-u", "-f", "x.log"],
    &["-u", "--renormalize", "f.txt"],
    &["-u", "--renormalize"],
    &["-A", "n.txt"],
    &["-A", "nosuch"],
    &["-A", "x.log"],
    &["-A", "-u"],
    &["-A", "-f"],
    &["--all", "."],
    &["-A", ":!n.txt"],
    &["--renormalize", "."],
    &["--renormalize", "g.txt"],
    &["--renormalize", "f.txt", "g.txt"],
    &["--renormalize", "f.txt"],
];

/// The status a command ended with and what it wrote to stderr, without
/// the advice, which names the program.
pub(crate) fn said(output: &Output) -> (Option<i32>, Vec<&BStr>) {
    let lines = output.stderr.split(|b| *b == b'\n');
    let kept = |line: &&[u8]| !line.starts_with(b"hint:") && !line.starts_with(b"request:");
    let lines = lines.filter(kept).map(|line| line.as_bstr());
    (output.status.code(), lines.collect())
}

/// The files of case `n`, made and committed just before it runs: a case
/// that stages in the whole tree leaves those of the cases to come alone.
fn files(trees: &Trees, n: usize) {
    for file in ["top.txt", "sub/f.txt", "sub/g.txt", "sub/deep/g.txt"] {
        trees.write(&format!("c{n}/{file}"), "base\n");
    }
    each(trees, &["add", &format!("c{n}")]);
    each(trees, &["commit", "-m", "case"]);
    for file in ["top.txt", "sub/f.txt", "sub/n.txt", "new.txt", "sub/x.log"] {
        trees.write(&format!("c{n}/{file}"), "now\n");
    }
    for tree in ["direct", "served", "git"] {
        std::fs::remove_file(trees.at(tree).join(format!("c{n}/sub/g.txt"))).unwrap();
    }
}

#[test]
fn add_without_a_path_and_with_update_or_all_does_what_git_does() {
    let trees = Trees::new();
    trees.write(".gitignore", "*.log\n");
    each(&trees, &["add", "."]);
    let mut ended = 0;
    for (n, args) in CASES.iter().enumerate() {
        files(&trees, n);
        let mut full = vec!["add"];
        full.extend(*args);
        let at = format!("c{n}/sub");
        let by_git = git(&trees.at("git").join(&at), &full);
        let direct = output(&trees.at("direct").join(&at), &full, false);
        let served = output(&trees.at("served").join(&at), &full, true);
        assert_eq!(said(&direct), said(&by_git), "{args:?}");
        assert_eq!(said(&served), said(&by_git), "{args:?}, served");
        let theirs = entries(&trees, &trees.at("git").join(".git/index"));
        for tree in ["direct", "served"] {
            let ours = entries(&trees, &trees.at(tree).join(".alt/index"));
            assert_eq!(ours.as_bstr(), theirs.as_bstr(), "{tree} after {args:?}");
        }
        ended += (by_git.status.code() == Some(128)) as usize;
        each(&trees, &["commit", "--allow-empty", "-m", "staged"]);
    }
    // the cases are what they are meant to be: git ends these
    assert_eq!(ended, 13);
}

#[test]
fn add_without_a_path_advises_as_the_settings_have_it() {
    let trees = Trees::new();
    let hints = |output: &Output, program: &str| -> Vec<String> {
        let said = output.stderr.to_str_lossy().replace(program, "<program>");
        let hints = said.lines().filter(|line| line.starts_with("hint:"));
        hints.map(str::to_owned).collect()
    };
    let by_git = hints(&git(&trees.at("git"), &["add"]), "git");
    assert_eq!(by_git.len(), 2);
    for (tree, daemon) in [("direct", false), ("served", true)] {
        assert_eq!(
            hints(&output(&trees.at(tree), &["add"], daemon), "alt"),
            by_git
        );
        let config = "[advice]\n\taddEmptyPathspec = false\n";
        write(&trees.at(tree), ".alt/git-import/config", config);
        let quiet = output(&trees.at(tree), &["add"], daemon);
        assert_eq!(quiet.stderr, b"Nothing specified, nothing added.\n");
    }
}

#[test]
fn an_absolute_path_with_a_name_missing_on_its_way_ends_add_as_it_ends_git() {
    let trees = Trees::new();
    let beside = trees.at("nosuch").join("tree/f.txt");
    let beside = beside.to_str().unwrap();
    let by_git = git(&trees.at("git"), &["add", beside]);
    assert_eq!(by_git.status.code(), Some(128));
    assert!(
        by_git.stderr.starts_with(b"fatal: Invalid path '"),
        "{by_git:?}"
    );
    for (tree, daemon) in [("direct", false), ("served", true)] {
        let ours = output(&trees.at(tree), &["add", beside], daemon);
        assert_eq!(said(&ours), said(&by_git), "{tree}");
    }
}
