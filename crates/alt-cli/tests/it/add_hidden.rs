//! `add` tells of a path an ignore rule hides where git does: when a path
//! given leads to it or into it as far as it is written out, tracked files
//! beneath it or not, and when a path that takes away names it. What it
//! says ends in a newline, and with `--json` the paths are in the details
//! of the error.

use std::process::Output;

use alt_bytes::{BStr, ByteSlice};

use crate::byte_names::{each, entries};
use crate::cwd_paths::{Trees, git};
use crate::scan_daemon::output;

/// Where each case runs beneath its own directory, and what it adds.
const CASES: &[(&str, &[&str])] = &[
    ("sub", &["."]),
    ("sub", &["f.txt"]),
    ("sub", &["f.txt", "n.txt"]),
    ("", &["sub"]),
    ("", &["sub/"]),
    ("", &["sub/deep"]),
    ("", &["sub/n.txt"]),
    ("", &["sub/*"]),
    ("", &["sub/de*"]),
    ("", &["x.lo*"]),
    ("", &[".", ":!x.log"]),
    ("", &[".", ":!sub"]),
    ("", &["top.txt", ":!x.log"]),
    ("", &[":!x.log"]),
    ("", &[".", ":(exclude)*.log"]),
    ("", &["x.log", "top.txt"]),
    ("", &["."]),
    ("sub", &[".", ":!../x.log"]),
    ("other", &["o.txt", ":!../x.log"]),
    ("other", &["o.txt", "../top.txt", ":!../x.log"]),
    ("", &["-f", "sub"]),
    ("", &["-f", ".", ":!x.log"]),
    ("", &["--renormalize", "sub"]),
];

/// The status a command ended with and what it wrote to stderr, without
/// the advice, which names the program.
fn said(output: &Output) -> (Option<i32>, Vec<&BStr>) {
    let lines = output.stderr.split(|b| *b == b'\n');
    let kept = |line: &&[u8]| !line.starts_with(b"hint:") && !line.starts_with(b"request:");
    let lines = lines.filter(kept).map(|line| line.as_bstr());
    (output.status.code(), lines.collect())
}

fn files(trees: &Trees) {
    for (n, _) in CASES.iter().enumerate() {
        for file in ["top.txt", "sub/f.txt", "sub/deep/g.txt", "other/o.txt"] {
            trees.write(&format!("c{n}/{file}"), "base\n");
        }
    }
    each(trees, &["add", "."]);
    each(trees, &["commit", "-m", "cases"]);
    for (n, _) in CASES.iter().enumerate() {
        trees.write(&format!("c{n}/.gitignore"), "/sub/\n*.log\n");
        for file in ["top.txt", "sub/f.txt", "sub/n.txt", "x.log", "other/o.txt"] {
            trees.write(&format!("c{n}/{file}"), "now\n");
        }
    }
}

#[test]
fn add_tells_of_what_an_ignore_rule_hides_where_git_does() {
    let trees = Trees::new();
    files(&trees);
    let mut told = 0;
    for (n, (at, paths)) in CASES.iter().enumerate() {
        let mut args = vec!["add"];
        args.extend(*paths);
        let at = format!("c{n}/{at}");
        let by_git = git(&trees.at("git").join(&at), &args);
        let direct = output(&trees.at("direct").join(&at), &args, false);
        let served = output(&trees.at("served").join(&at), &args, true);
        assert_eq!(said(&direct), said(&by_git), "{paths:?} in {at}");
        assert_eq!(said(&served), said(&by_git), "{paths:?} in {at}, served");
        assert!(direct.stderr.is_empty() || direct.stderr.ends_with(b"\n"));
        let theirs = entries(&trees, &trees.at("git").join(".git/index"));
        for tree in ["direct", "served"] {
            let ours = entries(&trees, &trees.at(tree).join(".alt/index"));
            assert_eq!(
                ours.as_bstr(),
                theirs.as_bstr(),
                "{tree}: {paths:?} in {at}"
            );
        }
        told += (by_git.status.code() == Some(1)) as usize;
    }
    // git tells of a hidden path in these, and in the others it does not
    assert_eq!(told, 16);
}

#[test]
fn add_names_what_it_left_alone_in_the_details_of_its_error() {
    let trees = Trees::new();
    trees.write(".gitignore", "*.log\n");
    trees.write("x.log", "x\n");
    trees.write("new.txt", "new\n");
    for (tree, daemon) in [("direct", false), ("served", true)] {
        let args = ["add", "--json", "x.log", "new.txt"];
        let ended = output(&trees.at(tree), &args, daemon);
        assert_eq!(ended.status.code(), Some(1), "{ended:?}");
        let error: alt_json::Value = alt_json::from_slice(&ended.stderr).unwrap();
        let error = &error["error"];
        assert_eq!(error["kind"], "paths_ignored");
        assert_eq!(
            error["details"]["ignored_paths"],
            alt_json::json!([{"path": "x.log"}])
        );
        let staged = entries(&trees, &trees.at(tree).join(".alt/index"));
        assert!(staged.windows(7).any(|w| w == b"new.txt"), "{tree}");
    }
}
