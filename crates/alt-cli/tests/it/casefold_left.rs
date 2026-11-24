//! What is left of names that meet on a file system that takes names
//! differing in case for one: a clone, a new workspace, a merge that
//! stops, and the directory a command asks the file system in.

use std::path::Path;

use crate::casefold::{clean, code, folded, place};
use crate::casefold_dirs::{read, told};
use crate::casefold_meet::{MEET, imported, nothing_pending, put, source};
use crate::casefold_wider::cut;
use crate::like_git::{alt, alt_daemon, git, heads, index, ok};

/// The paths a warning of paths that collided names, sorted.
fn warned(text: &str) -> Vec<String> {
    let at = text.find("warning: the following paths have collided");
    let text = &text[at.unwrap_or_else(|| panic!("no warning in: {text}"))..];
    let named = text.lines().filter(|l| l.starts_with("  '"));
    let mut named: Vec<String> = named.map(|l| l.trim().to_owned()).collect();
    named.sort();
    named
}

fn err(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_clone_says_which_paths_met_as_an_import_does() {
    let source = source();
    let root = source.path();
    // a file and a directory of one name, and two files of one name
    ok(git(root, &["switch", "-q", "both"]));
    put(root, "x", "small\n");
    put(root, "X", "great\n");
    ok(git(root, &["add", "-A"]));
    ok(git(root, &["commit", "-q", "-m", "two files"]));
    ok(git(root, &["branch", "-f", "main", "both"]));
    let url = crate::wire_test_server::spawn(root.to_owned());
    let held = alt_os::temp::tempdir_in(place()).unwrap();
    let met = ["'D/y'", "'X'", "'d'", "'x'"];
    let said = ok(alt(held.path(), &["clone", &url, "a"]));
    assert_eq!(warned(&said), met);
    let by_git = git(
        root,
        &["clone", "-q", ".", held.path().join("g").to_str().unwrap()],
    );
    assert!(by_git.status.success());
    assert_eq!(warned(&err(&by_git)), met);
    let (ours, theirs) = (held.path().join("a"), held.path().join("g"));
    assert_eq!(folded(&ours), folded(&theirs));
    assert_eq!(index(&ours), ok(git(&theirs, &["ls-files", "-s"])));
    nothing_pending(&ours);
    let state = told(&ok(alt(&ours, &["status"])));
    assert!(state.contains(&"deleted: D/y".to_owned()), "{state:?}");
    // the store is one to work in
    put(&ours, "keep", "worked in\n");
    ok(alt(&ours, &["add", "keep"]));
    ok(alt(&ours, &["commit", "-m", "worked in"]));
    // asked for JSON, the clone tells of the paths beside it
    let asked = alt(held.path(), &["clone", "--json", &url, "j"]);
    assert_eq!(code(&asked), Some(0), "{}", err(&asked));
    assert_eq!(warned(&err(&asked)), met);
    let answer = String::from_utf8_lossy(&asked.stdout).into_owned();
    let last = answer.lines().last().unwrap_or_default();
    assert!(
        last.starts_with('{') && !answer.contains("warning"),
        "{answer}"
    );
}

/// The workspace `name` is not made of `branch` at `tree`: the command
/// names the paths that meet, and nothing of a workspace is left.
fn not_made(repo: &Path, name: &str, tree: &Path, branch: &str) {
    let at = heads(repo);
    let out = alt(
        repo,
        &["workspace", "add", name, tree.to_str().unwrap(), branch],
    );
    assert_eq!(code(&out), Some(1), "{}", err(&out));
    assert!(err(&out).contains(MEET), "{}", err(&out));
    assert!(!ok(alt(repo, &["workspace", "list"])).contains(name));
    assert!(!repo.join(".alt/workspaces").join(name).exists());
    let left: Vec<_> = std::fs::read_dir(tree).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(heads(repo), at);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_workspace_is_not_made_of_a_tree_whose_names_meet() {
    let source = source();
    // the store and the tree both where names fold
    let (held, repo, _) = imported(source.path(), "main");
    let tree = held.path().join("tree");
    not_made(&repo, "ws", &tree, "both");
    not_made(&repo, "ws", &tree, "deep");
    // the branch is free, and the place: a tree that can stand is made there
    let add = ["workspace", "add", "ws", tree.to_str().unwrap(), "only-dir"];
    ok(alt(&repo, &add));
    assert_eq!(read(&tree, "D/y"), "y\n");
    clean(&tree, false);
    // the store where case is told apart, the tree where it is not
    ok(git(source.path(), &["switch", "-q", "main"]));
    let plain = alt_os::temp::tempdir().unwrap();
    let apart = plain.path().join("r");
    ok(alt(source.path(), &["import", apart.to_str().unwrap()]));
    let folding = alt_os::temp::tempdir_in(place()).unwrap();
    not_made(&apart, "ws", &folding.path().join("tree"), "both");
    // and the other way round, where both paths have their own names
    let tree = plain.path().join("tree");
    let add = ["workspace", "add", "apart", tree.to_str().unwrap(), "both"];
    ok(alt(&repo, &add));
    assert_eq!(read(&tree, "d"), "file\n");
    assert_eq!(read(&tree, "D/y"), "y\n");
    clean(&tree, false);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn what_a_command_cut_short_left_to_ask_in_is_no_part_of_the_tree_and_goes() {
    let source = source();
    let root = source.path();
    // a branch that brings two names into one directory: they are asked about
    ok(git(root, &["switch", "-q", "-c", "two", "main"]));
    put(root, "p", "p\n");
    put(root, "q", "q\n");
    ok(git(root, &["add", "-A"]));
    ok(git(root, &["commit", "-q", "-m", "two"]));
    ok(git(root, &["switch", "-q", "-c", "named", "main"]));
    put(root, ".alt-names-asked/x", "x\n");
    ok(git(root, &["add", "-A"]));
    ok(git(root, &["commit", "-q", "-m", "named"]));
    let (held, repo, _) = imported(root, "main");
    ok(alt(&repo, &["branch", "side", "main"]));
    let tree = held.path().join("tree");
    let add = ["workspace", "add", "ws", tree.to_str().unwrap(), "side"];
    ok(alt(&repo, &add));
    let left = tree.join(".alt-names-asked");
    // cut short while it asks
    let list = held.path().join("points");
    let whole = cut(
        &tree,
        &["merge", "two"],
        &format!("list:{}", list.display()),
    );
    assert_eq!(code(&whole), Some(0), "{}", err(&whole));
    let points = std::fs::read_to_string(&list).unwrap();
    let asks = points.lines().position(|point| point == "names asked");
    let asks = asks.unwrap_or_else(|| panic!("{points}")) + 1;
    assert!(!left.exists());
    ok(alt(&tree, &["reset", "--hard", "main"]));
    let cut_short = || {
        let killed = cut(&tree, &["merge", "two"], &asks.to_string());
        assert_eq!(code(&killed), None);
        assert!(left.join("p").exists());
    };
    cut_short();
    assert_eq!(ok(alt(&tree, &["operation", "pending"])), "");
    // what it left is no part of the tree
    clean(&tree, false);
    clean(&tree, true);
    // a command that writes clears it away, though it asks nothing
    ok(alt(&tree, &["add", "-A"]));
    assert!(!left.exists());
    clean(&tree, false);
    cut_short();
    let none = ok(alt(&tree, &["stash", "-u"]));
    assert!(none.contains("No local changes to save"), "{none}");
    assert_eq!(ok(alt(&repo, &["stash", "list"])), "");
    // and no path is checked out or staged under its name
    cut_short();
    let named = alt(&tree, &["add", ".alt-names-asked/p"]);
    assert_eq!(code(&named), Some(128), "{}", err(&named));
    let named = alt(&tree, &["merge", "named"]);
    assert_eq!(code(&named), Some(128));
    assert!(err(&named).contains("invalid path"), "{}", err(&named));
    clean(&tree, false);
    ok(alt(&tree, &["merge", "two"]));
    assert_eq!(read(&tree, "q"), "q\n");
    assert!(!left.exists());
    clean(&tree, false);
}

/// The command fails with `why`, at 1, and leaves everything as it is.
fn stopped(repo: &Path, command: &[&str], why: &str) {
    let (at, staged, tree) = (heads(repo), index(repo), folded(repo));
    let out = alt(repo, command);
    assert_eq!(code(&out), Some(1), "{command:?}: {}", err(&out));
    assert!(err(&out).contains(why), "{command:?}: {}", err(&out));
    assert_eq!(heads(repo), at, "{command:?}");
    assert_eq!(index(repo), staged, "{command:?}");
    assert_eq!(folded(repo), tree, "{command:?}");
    nothing_pending(repo);
    clean(repo, false);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn a_merge_that_stops_puts_no_file_and_directory_in_each_other_s_place() {
    const PLACE: &str = "in each other's place";
    let source = source();
    let root = source.path();
    let commit = |message: &str| {
        ok(git(root, &["add", "-A"]));
        ok(git(root, &["commit", "-q", "-m", message]));
    };
    // from the file: one line changes `keep`, the other gives the name to
    // a directory and then changes `keep` its own way
    ok(git(root, &["switch", "-q", "-c", "mine", "only-file"]));
    put(root, "keep", "mine\n");
    commit("mine");
    ok(git(root, &["switch", "-q", "-c", "swapped", "only-file"]));
    std::fs::remove_file(root.join("d")).unwrap();
    put(root, "D/y", "y\n");
    commit("swap");
    put(root, "keep", "swapped\n");
    commit("and keep");
    // and one commit that does both
    ok(git(root, &["switch", "-q", "-c", "at-once", "only-file"]));
    std::fs::remove_file(root.join("d")).unwrap();
    put(root, "D/y", "y\n");
    put(root, "keep", "at once\n");
    commit("both at once");

    // a tree that would hold both is written by no merge that stops
    let (_held, repo, _) = imported(root, "ours");
    stopped(&repo, &["merge", "theirs"], MEET);
    assert_eq!(read(&repo, "D/y"), "y\n");
    assert_eq!(read(&repo, "keep"), "ours\n");

    // nor is one that gives the name of a file to a directory
    let (_held, repo, _) = imported(root, "mine");
    stopped(&repo, &["merge", "swapped"], PLACE);
    stopped(&repo, &["cherry-pick", "at-once"], PLACE);
    assert_eq!(read(&repo, "d"), "file\n");
    // the commit that gives the name away merges alone, and what is left
    // of the line then stops over `keep` as any merge does
    ok(alt(&repo, &["merge", "swapped~1"]));
    assert_eq!(read(&repo, "D/y"), "y\n");
    assert!(!index(&repo).contains("\td\n"));
    clean(&repo, false);
    let out = alt(&repo, &["merge", "swapped"]);
    assert_eq!(code(&out), Some(1), "{}", err(&out));
    assert!(!err(&out).contains(PLACE) && !err(&out).contains(MEET));
    let state = ok(alt_daemon(&repo, &["status"]));
    assert!(state.contains("keep"), "{state}");
    assert!(read(&repo, "keep").contains("<<<<<<<"));
    assert_eq!(read(&repo, "D/y"), "y\n");
}
