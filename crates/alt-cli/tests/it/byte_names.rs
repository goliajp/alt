//! A file whose name is not UTF-8 is a file like any other in the tree:
//! `add`, `status`, `diff`, `commit` and `switch` take it as git does,
//! and where a path is shown it is quoted as git quotes it.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use alt_bytes::{BStr, ByteSlice};

use crate::cwd_paths::{Trees, git};
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
fn alt(trees: &Trees, args: &[&str]) -> Vec<u8> {
    let said = |tree: &str, daemon: bool| {
        let o = output(&trees.at(tree), args, daemon);
        assert!(o.status.success(), "{args:?} in {tree}: {o:?}");
        o.stdout
    };
    let direct = said("direct", false);
    assert_eq!(said("served", true).as_bstr(), direct.as_bstr(), "{args:?}");
    direct
}

fn by_git(trees: &Trees, args: &[&str]) -> Vec<u8> {
    let o = git(&trees.at("git"), args);
    assert!(o.status.success(), "git {args:?}: {o:?}");
    o.stdout
}

/// The lines of a `status` but for git's advice, byte for byte.
fn listed(status: &[u8]) -> Vec<&BStr> {
    let advice: [&[u8]; 5] = [
        b"  (",
        b"no changes added",
        b"nothing added to commit",
        b"Untracked files not listed",
        b"You have unmerged",
    ];
    let kept = |line: &&[u8]| !line.is_empty() && !advice.iter().any(|a| line.starts_with(a));
    let lines = status.split(|b| *b == b'\n').filter(kept);
    lines.map(|line| line.as_bstr()).collect()
}

fn has(said: &[u8], part: &[u8]) -> bool {
    said.windows(part.len()).any(|w| w == part)
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
        let (ours, theirs) = (alt(trees, args), by_git(trees, args));
        assert_eq!(ours.as_bstr(), theirs.as_bstr(), "{args:?} {at}");
    }
    let theirs = entries(trees, &trees.at("git").join(".git/index"));
    for tree in ["direct", "served"] {
        let ours = entries(trees, &trees.at(tree).join(".alt/index"));
        assert_eq!(ours.as_bstr(), theirs.as_bstr(), "the index of {tree} {at}");
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
    assert!(has(&alt(&trees, &["status"]), b"\t\"b\\377\\376.txt\"\n"));
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
}

#[test]
fn a_name_is_shown_unquoted_where_the_settings_say_so() {
    let trees = Trees::new();
    // but for what no name can hold
    write(&trees, b"n\xffew.txt", "new\n");
    write(&trees, "na\u{ef}ve \"x\".txt".as_bytes(), "new\n");
    for tree in ["direct", "served"] {
        let config = "[core]\n\tquotePath = false\n";
        crate::scan_daemon::write(&trees.at(tree), ".alt/git-import/config", config);
    }
    let set = ["config", "core.quotePath", "false"];
    assert!(git(&trees.at("git"), &set).status.success());
    alike(&trees, "unquoted");
    assert!(has(&alt(&trees, &["status"]), b"\tn\xffew.txt\n"));
}

/// The lines of what a command wrote that begin with `start`.
fn lines<'a>(said: &'a [u8], start: &[u8]) -> Vec<&'a BStr> {
    let lines = said.split(|b| *b == b'\n');
    let kept = lines.filter(|line| line.starts_with(start));
    kept.map(|line| line.as_bstr()).collect()
}

#[test]
fn a_repository_at_a_name_that_is_not_utf8_is_warned_of_by_its_bytes() {
    let trees = Trees::new();
    for tree in TREES {
        let inner = trees.at(tree).join(OsStr::from_bytes(b"r\xfc"));
        std::fs::create_dir(&inner).unwrap();
        assert!(git(&inner, &["init", "-q", "-b", "main"]).status.success());
        let commit = ["commit", "-q", "--allow-empty", "-m", "inner"];
        assert!(git(&inner, &commit).status.success());
    }
    let warned = git(&trees.at("git"), &["add", "."]).stderr;
    assert_eq!(
        lines(&warned, b"warning:"),
        [b"warning: adding embedded git repository: r\xfc".as_bstr()]
    );
    for (tree, daemon) in [("direct", false), ("served", true)] {
        let ours = output(&trees.at(tree), &["add", "."], daemon).stderr;
        assert_eq!(
            lines(&ours, b"warning:"),
            lines(&warned, b"warning:"),
            "{tree}"
        );
    }
}

#[test]
fn a_conflict_at_a_name_that_is_not_utf8_is_said_of_its_bytes() {
    let trees = Trees::new();
    write(&trees, b"n\xff.txt", "base\n");
    each(&trees, &["add", "."]);
    each(&trees, &["commit", "-m", "name"]);
    each(&trees, &["switch", "-c", "side"]);
    write(&trees, b"n\xff.txt", "side\n");
    each(&trees, &["add", "."]);
    each(&trees, &["commit", "-m", "side"]);
    each(&trees, &["switch", "main"]);
    write(&trees, b"n\xff.txt", "main\n");
    assert!(has(
        &alt(&trees, &["diff", "--json"]),
        b"\"path_bytes\":\"6eff2e747874\""
    ));
    each(&trees, &["add", "."]);
    assert!(has(
        &alt(&trees, &["status", "--json"]),
        b"\"path_bytes\":\"6eff2e747874\",\"change\""
    ));
    each(&trees, &["commit", "-m", "main"]);
    let theirs = git(&trees.at("git"), &["merge", "side"]).stdout;
    let conflict = lines(&theirs, b"CONFLICT");
    assert_eq!(
        conflict,
        [b"CONFLICT (content): Merge conflict in n\xff.txt".as_bstr()]
    );
    for (tree, daemon) in [("direct", false), ("served", true)] {
        let ours = output(&trees.at(tree), &["merge", "side"], daemon).stdout;
        assert_eq!(lines(&ours, b"CONFLICT"), conflict, "{tree}");
    }
    let unmerged = b"\"unmerged\":[\"n\xef\xbf\xbd.txt\"],\"unmerged_bytes\":[{\"index\":0,\"path_bytes\":\"6eff2e747874\"}]";
    let status = output(&trees.at("direct"), &["status", "--json"], false).stdout;
    assert!(has(&status, unmerged), "{}", status.as_bstr());
    let served = output(&trees.at("served"), &["status", "--json"], true).stdout;
    assert_eq!(served.as_bstr(), status.as_bstr());
    for args in [&["status"][..], &["status", "-uall"]] {
        let ours = output(&trees.at("direct"), args, false).stdout;
        assert_eq!(listed(&ours), listed(&by_git(&trees, args)), "{args:?}");
    }
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
    let has = |name: &[u8]| has(&staged, name);
    assert!(has(b"k\xff.txt") && !has(b"b\xff.txt") && !has(b"\xfe.log") && !has(b"in.txt"));
}
