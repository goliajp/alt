//! Three trees, for alt run directly, alt through its daemon and git,
//! with two branches whose merge stops, and what the tests of a stopped
//! merge or pick compare of them.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::byte_names::entries;
use crate::cwd_paths::{Trees, git};
use crate::scan_daemon::output;

pub(crate) const TREES: [&str; 3] = ["direct", "served", "git"];

/// The exit code of `args` in each tree.
pub(crate) fn every(trees: &Trees, args: &[&str]) -> [Option<i32>; 3] {
    [
        output(&trees.at("direct"), args, false).status.code(),
        output(&trees.at("served"), args, true).status.code(),
        git(&trees.at("git"), args).status.code(),
    ]
}

/// Whether `args` succeeded in each tree. A refusal has one exit code in
/// git and another in alt.
pub(crate) fn succeeded(trees: &Trees, args: &[&str]) -> [bool; 3] {
    every(trees, args).map(|code| code == Some(0))
}

pub(crate) fn commit(trees: &Trees, message: &str) {
    assert_eq!(every(trees, &["add", "."]), [Some(0); 3]);
    assert_eq!(every(trees, &["commit", "-m", message]), [Some(0); 3]);
}

pub(crate) fn executable(trees: &Trees, file: &str, on: bool) {
    for tree in TREES {
        let mode = if on { 0o755 } else { 0o644 };
        let permissions = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(trees.at(tree).join(file), permissions).unwrap();
    }
}

pub(crate) fn remove(trees: &Trees, file: &str) {
    for tree in TREES {
        std::fs::remove_file(trees.at(tree).join(file)).unwrap();
    }
}

/// A branch `side` and a branch `main` that part after one commit. Merging
/// or picking `side` on `main` stops on `f.txt`, which both changed, and on
/// `both.txt`, which both added. From `side` alone come a change of
/// `t.txt`, the new `new.txt` and `in/a/dir/n.txt`, `d.txt` deleted and
/// `x.sh` no longer executable. Neither touches `u.txt` and `keep.sh`.
pub(crate) fn diverged() -> Trees {
    let trees = Trees::new();
    trees.write("f.txt", "1\n2\n3\n");
    for file in ["t.txt", "u.txt", "d.txt", "x.sh", "keep.sh"] {
        trees.write(file, &format!("{file}\n"));
    }
    executable(&trees, "x.sh", true);
    executable(&trees, "keep.sh", true);
    commit(&trees, "files");
    assert_eq!(every(&trees, &["switch", "-c", "side"]), [Some(0); 3]);
    trees.write("f.txt", "1\nside\n3\n");
    trees.write("t.txt", "t.txt by side\n");
    trees.write("new.txt", "new\n");
    trees.write("in/a/dir/n.txt", "n\n");
    trees.write("both.txt", "side\n");
    remove(&trees, "d.txt");
    executable(&trees, "x.sh", false);
    commit(&trees, "side");
    assert_eq!(every(&trees, &["switch", "main"]), [Some(0); 3]);
    trees.write("f.txt", "1\nmain\n3\n");
    trees.write("both.txt", "main\n");
    commit(&trees, "main");
    trees
}

/// Every file beneath `dir` but those of the repository itself: its
/// content, or where it links to, and whether it is executable.
fn files(root: &Path, dir: &Path, found: &mut BTreeMap<String, (Vec<u8>, bool)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path
            .strip_prefix(root)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let meta = path.symlink_metadata().unwrap();
        if name == ".alt" || name == ".git" {
            continue;
        } else if meta.is_dir() {
            // a directory left empty is a difference too
            found.insert(format!("{name}/"), (Vec::new(), false));
            files(root, &path, found);
        } else if meta.is_symlink() {
            let to = std::fs::read_link(&path).unwrap();
            found.insert(name, (to.to_str().unwrap().into(), false));
        } else {
            let executable = meta.permissions().mode() & 0o100 != 0;
            found.insert(name, (std::fs::read(&path).unwrap(), executable));
        }
    }
}

pub(crate) fn working_tree(trees: &Trees, tree: &str) -> BTreeMap<String, (Vec<u8>, bool)> {
    let mut found = BTreeMap::new();
    files(&trees.at(tree), &trees.at(tree), &mut found);
    found
}

pub(crate) fn head(trees: &Trees, tree: &str) -> String {
    let at = trees.at(tree);
    let said = match tree {
        "git" => git(&at, &["rev-parse", "HEAD"]),
        _ => output(&at, &["rev-parse", "HEAD"], false),
    };
    assert!(said.status.success());
    String::from_utf8(said.stdout).unwrap()
}

/// The index and the working tree of both alt trees are those of git's.
pub(crate) fn alike(trees: &Trees, when: &str) {
    let theirs = entries(trees, &trees.at("git").join(".git/index"));
    for tree in ["direct", "served"] {
        let ours = entries(trees, &trees.at(tree).join(".alt/index"));
        assert_eq!(
            String::from_utf8_lossy(&ours),
            String::from_utf8_lossy(&theirs),
            "the index of {tree} {when}"
        );
        let shown = |tree: &str| -> Vec<(String, String, bool)> {
            let files = working_tree(trees, tree).into_iter();
            files
                .map(|(name, (content, bit))| {
                    (name, String::from_utf8_lossy(&content).into_owned(), bit)
                })
                .collect()
        };
        assert_eq!(shown(tree), shown("git"), "the files of {tree} {when}");
    }
}

/// The operation `status --json` tells of in the alt trees.
pub(crate) fn operation(trees: &Trees) -> String {
    let said = |tree: &str, daemon: bool| {
        let status = output(&trees.at(tree), &["status", "--json"], daemon);
        let status = String::from_utf8(status.stdout).unwrap();
        let from = status.find(r#""operation":"#).unwrap();
        let rest = &status[from + r#""operation":"#.len()..];
        match rest.starts_with('{') {
            true => rest[..=rest.find('}').unwrap()].to_owned(),
            false => rest[..rest.find(',').unwrap()].to_owned(),
        }
    };
    let direct = said("direct", false);
    assert_eq!(said("served", true), direct);
    direct
}
