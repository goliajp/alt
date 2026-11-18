//! A file may have for its name what a path given to `add` is written
//! as, wildcards and all. Git goes through what is not tracked in order,
//! and once such a path is all of a name it tries it on no name after:
//! so does `add`, here beside git and by its daemon.

use alt_bytes::ByteSlice;

use crate::add_modes::said;
use crate::byte_names::{each, entries};
use crate::cwd_paths::{Trees, git};
use crate::scan_daemon::output;

/// Where each case runs beneath its own directory, and what it adds.
const CASES: &[(&str, &[&str])] = &[
    ("sub", &["*.txt"]),
    ("sub", &["\\*.txt"]),
    ("sub", &[":(literal)*.txt"]),
    ("sub", &["*.txt", "n.txt"]),
    ("sub", &["n.txt", "*.txt"]),
    ("sub", &["*.txt", "*.txt"]),
    ("sub", &["*.tx?"]),
    ("sub", &[":(glob)*.txt"]),
    ("sub", &[":(icase)*.TXT"]),
    ("", &["sub/*.txt"]),
    ("sub", &[".", ":!*.txt"]),
    ("sub", &["-u", "*.txt"]),
];

#[test]
fn a_path_that_is_all_of_a_name_names_no_later_file_that_is_not_tracked() {
    let trees = Trees::new();
    for (n, _) in CASES.iter().enumerate() {
        trees.write(&format!("c{n}/sub/f.txt"), "base\n");
        trees.write(&format!("c{n}/sub/gone.txt"), "base\n");
    }
    each(&trees, &["add", "."]);
    each(&trees, &["commit", "-m", "cases"]);
    for (n, (at, paths)) in CASES.iter().enumerate() {
        for file in ["!a.txt", "*.txt", "f.txt", "n.txt", "deep/n3.txt", "N2.TXT"] {
            trees.write(&format!("c{n}/sub/{file}"), "now\n");
        }
        for tree in ["direct", "served", "git"] {
            std::fs::remove_file(trees.at(tree).join(format!("c{n}/sub/gone.txt"))).unwrap();
        }
        let mut args = vec!["add"];
        args.extend(*paths);
        let at = format!("c{n}/{at}");
        let by_git = git(&trees.at("git").join(&at), &args);
        let direct = output(&trees.at("direct").join(&at), &args, false);
        let served = output(&trees.at("served").join(&at), &args, true);
        assert_eq!(said(&direct), said(&by_git), "{paths:?}");
        assert_eq!(said(&served), said(&by_git), "{paths:?}, served");
        let theirs = entries(&trees, &trees.at("git").join(".git/index"));
        for tree in ["direct", "served"] {
            let ours = entries(&trees, &trees.at(tree).join(".alt/index"));
            assert_eq!(ours.as_bstr(), theirs.as_bstr(), "{tree}: {paths:?}");
        }
    }
}
