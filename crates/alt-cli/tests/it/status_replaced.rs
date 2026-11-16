//! A directory where the index has a file: `status` tells of the file as
//! deleted and, as git does, not of the directory, whose files it lists
//! where every untracked file is asked for.

use crate::cwd_paths::{Trees, git, listed};
use crate::scan_daemon::{run, write};

#[test]
fn a_directory_in_place_of_a_tracked_file_is_no_untracked_path() {
    let trees = Trees::new();
    for tree in ["direct", "served", "git"] {
        let root = trees.at(tree);
        for (file, inside) in [("top.txt", "n"), ("sub/f.txt", "deep/x")] {
            std::fs::remove_file(root.join(file)).unwrap();
            write(&root, &format!("{file}/{inside}"), "new\n");
        }
        // only what an ignore rule hides is in this one
        std::fs::remove_file(root.join("other/o.txt")).unwrap();
        write(&root, "other/o.txt/x.log", "hidden\n");
        write(&root, ".gitignore", "*.log\n");
        write(&root, "new/n.txt", "new\n");
    }
    let all: [&[&str]; 3] = [&["status"], &["status", "-uall"], &["status", "-uno"]];
    for args in all {
        let theirs = git(&trees.at("git"), args);
        let theirs = listed(&String::from_utf8(theirs.stdout).unwrap());
        for (tree, daemon) in [("direct", false), ("served", true)] {
            let ours = run(&trees.at(tree), args, daemon);
            assert_eq!(listed(&ours), theirs, "{tree}: {args:?}");
        }
    }
    let short = git(&trees.at("git"), &["status", "--porcelain"]);
    let short = String::from_utf8(short.stdout).unwrap();
    let theirs: Vec<&str> = short.lines().filter_map(|l| l.strip_prefix("?? ")).collect();
    assert_eq!(theirs, [".gitignore", "new/"]);
    for (tree, daemon) in [("direct", false), ("served", true)] {
        let ours = run(&trees.at(tree), &["status", "--json"], daemon);
        let ours: alt_json::Value = alt_json::from_slice(ours.as_bytes()).unwrap();
        assert_eq!(ours["untracked"], alt_json::json!([".gitignore", "new/"]), "{tree}");
    }
}
