//! Three trees, for alt run directly, alt through its daemon and git,
//! and what the tests of a stopped step compare of them.

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

pub(crate) fn commit(trees: &Trees, message: &str) {
    assert_eq!(every(trees, &["add", "."]), [Some(0); 3]);
    assert_eq!(every(trees, &["commit", "-m", message]), [Some(0); 3]);
}

pub(crate) fn remove(trees: &Trees, file: &str) {
    for tree in TREES {
        std::fs::remove_file(trees.at(tree).join(file)).unwrap();
    }
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
