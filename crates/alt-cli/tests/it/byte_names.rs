//! A file whose name is not UTF-8 is a file like any other in the tree:
//! `add`, `status`, `diff`, `commit` and `switch` take it as git does,
//! and where a path is shown it is quoted as git quotes it.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::cwd_paths::{Trees, git, listed};
use crate::scan_daemon::{output, run};

const TREES: [&str; 3] = ["direct", "served", "git"];

fn write(trees: &Trees, name: &[u8], body: impl AsRef<[u8]>) {
    for tree in TREES {
        let file = trees.at(tree).join(OsStr::from_bytes(name));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body.as_ref()).unwrap();
    }
}

/// What `alt` wrote for `args`, the same by its daemon and without it.
fn alt(trees: &Trees, args: &[&str]) -> String {
    let said = |tree: &str, daemon: bool| {
        let o = output(&trees.at(tree), args, daemon);
        assert!(o.status.success(), "{args:?} in {tree}: {o:?}");
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    let direct = said("direct", false);
    assert_eq!(said("served", true), direct, "{args:?}");
    direct
}

fn by_git(trees: &Trees, args: &[&str]) -> String {
    let o = git(&trees.at("git"), args);
    assert!(o.status.success(), "git {args:?}: {o:?}");
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Every entry of the index at `index`, as git lists it byte for byte.
pub(crate) fn entries(trees: &Trees, index: &Path) -> Vec<u8> {
    let listed = std::process::Command::new("git")
        .current_dir(trees.at("git"))
        .env("GIT_INDEX_FILE", index)
        .args(["ls-files", "-s", "-z"])
        .output()
        .unwrap();
    assert!(listed.status.success());
    listed.stdout
}

fn alike(trees: &Trees, at: &str) {
    for args in [&["status"][..], &["status", "-uall"]] {
        let (ours, theirs) = (alt(trees, args), by_git(trees, args));
        assert_eq!(listed(&ours), listed(&theirs), "{args:?} {at}");
    }
    for args in [&["diff"][..], &["diff", "--cached"]] {
        assert_eq!(alt(trees, args), by_git(trees, args), "{args:?} {at}");
    }
    let theirs = entries(trees, &trees.at("git").join(".git/index"));
    for tree in ["direct", "served"] {
        let ours = entries(trees, &trees.at(tree).join(".alt/index"));
        assert_eq!(ours, theirs, "the index of {tree} {at}");
    }
}

pub(crate) fn each(trees: &Trees, args: &[&str]) {
    run(&trees.at("direct"), args, false);
    run(&trees.at("served"), args, true);
    assert!(git(&trees.at("git"), args).status.success(), "git {args:?}");
}

#[test]
fn a_name_that_is_not_utf8_is_staged_shown_committed_and_checked_out() {
    let trees = Trees::new();
    write(&trees, b"b\xff\xfe.txt", "bytes\n");
    write(&trees, b"d\xff/in.txt", "in\n");
    write(&trees, "caf\u{e9}.txt".as_bytes(), "text\n");
    write(&trees, b"quo\"te\\.txt", "marks\n");
    write(&trees, b"top.txt", "changed\n");
    alike(&trees, "new");
    assert!(alt(&trees, &["status"]).contains("\t\"b\\377\\376.txt\"\n"));
    each(&trees, &["add", "."]);
    alike(&trees, "staged");
    each(&trees, &["commit", "-m", "names"]);
    write(&trees, b"b\xff\xfe.txt", "bytes\nmore\n");
    write(&trees, b"d\xff/in.txt", "in\nmore\n");
    alike(&trees, "changed");
    each(&trees, &["add", "."]);
    each(&trees, &["commit", "-m", "more"]);
    each(&trees, &["branch", "side"]);
    each(&trees, &["switch", "side"]);
    write(&trees, b"b\xff\xfe.txt", "on the side\n");
    write(&trees, b"d\xff/x\xe9", "new on the side\n");
    for tree in TREES {
        let gone = trees.at(tree).join(OsStr::from_bytes(b"d\xff/in.txt"));
        std::fs::remove_file(gone).unwrap();
    }
    alike(&trees, "on the side");
    each(&trees, &["add", "."]);
    each(&trees, &["commit", "-m", "side"]);
    each(&trees, &["switch", "main"]);
    alike(&trees, "back on main");
    for tree in TREES {
        let at = |name: &[u8]| trees.at(tree).join(OsStr::from_bytes(name));
        assert_eq!(
            std::fs::read(at(b"b\xff\xfe.txt")).unwrap(),
            b"bytes\nmore\n"
        );
        assert!(at(b"d\xff/in.txt").exists(), "{tree}");
        assert!(!at(b"d\xff/x\xe9").exists(), "{tree}");
    }
    each(&trees, &["switch", "side"]);
    alike(&trees, "on the side again");
    // unquoted where the settings say so, but for what no name can hold
    write(&trees, b"n\xffew.txt", "new\n");
    write(&trees, "na\u{ef}ve \"x\".txt".as_bytes(), "new\n");
    for tree in ["direct", "served"] {
        let config = "[core]\n\tquotePath = false\n";
        crate::scan_daemon::write(&trees.at(tree), ".alt/git-import/config", config);
    }
    let set = ["config", "core.quotePath", "false"];
    assert!(git(&trees.at("git"), &set).status.success());
    alike(&trees, "unquoted");
}

#[test]
fn a_rule_that_is_not_utf8_hides_and_converts_what_it_names() {
    let trees = Trees::new();
    write(&trees, b".gitignore", b"b\xff.txt\n*\xfe.log\n/d\xff/\n");
    write(
        &trees,
        b".gitattributes",
        b"a\xff.txt text\n*\xfd.md text\n",
    );
    for hidden in [&b"b\xff.txt"[..], b"x\xfe.log", b"d\xff/in.txt"] {
        write(&trees, hidden, "hidden\n");
    }
    write(&trees, b"k\xff.txt", "kept\n");
    write(&trees, b"a\xff.txt", "one\r\ntwo\r\n");
    write(&trees, b"w\xfd.md", "one\r\ntwo\r\n");
    write(&trees, b"raw.md", "one\r\ntwo\r\n");
    alike(&trees, "new");
    each(&trees, &["add", "."]);
    alike(&trees, "staged");
    let staged = entries(&trees, &trees.at("direct").join(".alt/index"));
    let has = |name: &[u8]| staged.windows(name.len()).any(|w| w == name);
    assert!(has(b"k\xff.txt") && !has(b"b\xff.txt") && !has(b"\xfe.log") && !has(b"in.txt"));
}
