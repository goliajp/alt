//! A command that moves a ref and writes files refuses before the ref
//! moves: what it finds in its way leaves the repository as it was.

use crate::like_git::{alt, ok};

fn repo() -> alt_os::temp::TempDir {
    let dir = alt_os::temp::tempdir().unwrap();
    ok(alt(dir.path(), &["init", "."]));
    dir
}

fn commit(root: &std::path::Path, message: &str) {
    ok(alt(root, &["add", "-A"]));
    ok(alt(root, &["commit", "-m", message]));
}

#[test]
fn a_hard_reset_a_directory_is_in_the_way_of_moves_no_branch() {
    let dir = repo();
    let root = dir.path();
    std::fs::write(root.join("a"), "file\n").unwrap();
    commit(root, "one");
    std::fs::remove_file(root.join("a")).unwrap();
    std::fs::create_dir(root.join("a")).unwrap();
    std::fs::write(root.join("a/b"), "b\n").unwrap();
    commit(root, "two");
    let head = ok(alt(root, &["rev-parse", "HEAD"]));
    std::fs::write(root.join("a/u"), "u\n").unwrap();

    let refused = alt(root, &["reset", "--hard", "HEAD~1"]);
    assert!(!refused.status.success());
    assert_eq!(ok(alt(root, &["rev-parse", "HEAD"])), head);
    assert_eq!(std::fs::read_to_string(root.join("a/b")).unwrap(), "b\n");
    // nothing is left to finish: the next command runs
    std::fs::write(root.join("c"), "c\n").unwrap();
    ok(alt(root, &["add", "c"]));
    std::fs::remove_file(root.join("a/u")).unwrap();
    ok(alt(root, &["reset", "--hard", "HEAD~1"]));
    assert_eq!(std::fs::read_to_string(root.join("a")).unwrap(), "file\n");
}

#[test]
fn a_stash_a_directory_is_in_the_way_of_makes_no_entry() {
    let dir = repo();
    let root = dir.path();
    std::fs::write(root.join("a"), "file\n").unwrap();
    std::fs::write(root.join("b"), "b\n").unwrap();
    commit(root, "one");
    std::fs::remove_file(root.join("a")).unwrap();
    std::fs::create_dir(root.join("a")).unwrap();
    std::fs::write(root.join("a/u"), "u\n").unwrap();
    std::fs::write(root.join("b"), "bb\n").unwrap();

    let refused = alt(root, &["stash"]);
    assert!(!refused.status.success());
    assert_eq!(ok(alt(root, &["stash", "list"])), "");
    assert_eq!(std::fs::read_to_string(root.join("b")).unwrap(), "bb\n");
    ok(alt(root, &["add", "b"]));
    std::fs::remove_file(root.join("a/u")).unwrap();
    ok(alt(root, &["stash"]));
    assert_eq!(ok(alt(root, &["stash", "list"])).lines().count(), 1);
}

/// `main` holds `a/b` where its first commit and the branch `ahead` hold
/// the file `a`; an untracked `a/u` stands in the way of that file.
fn directory_in_the_way() -> alt_os::temp::TempDir {
    let dir = repo();
    let root = dir.path();
    std::fs::write(root.join("a"), "file\n").unwrap();
    std::fs::write(root.join("k"), "k\n").unwrap();
    commit(root, "one");
    std::fs::remove_file(root.join("a")).unwrap();
    std::fs::create_dir(root.join("a")).unwrap();
    std::fs::write(root.join("a/b"), "b\n").unwrap();
    commit(root, "two");
    ok(alt(root, &["switch", "-c", "ahead"]));
    std::fs::remove_dir_all(root.join("a")).unwrap();
    std::fs::write(root.join("a"), "file again\n").unwrap();
    commit(root, "three");
    ok(alt(root, &["switch", "main"]));
    std::fs::write(root.join("a/u"), "u\n").unwrap();
    dir
}

fn told(root: &std::path::Path) -> String {
    [
        ok(alt(root, &["rev-parse", "HEAD", "main", "ahead"])),
        ok(alt(root, &["rev-parse", "--abbrev-ref", "HEAD"])),
        ok(alt(root, &["branch"])),
        ok(alt(root, &["status"])),
        std::fs::read_to_string(root.join("a/u")).unwrap(),
        std::fs::read_to_string(root.join("a/b")).unwrap(),
    ]
    .join("\n--\n")
}

#[test]
fn a_command_a_directory_is_in_the_way_of_moves_no_ref_and_leaves_nothing_to_finish() {
    let dir = directory_in_the_way();
    let root = dir.path();
    let before = told(root);
    let commands: [&[&str]; 11] = [
        &["switch", "ahead"],
        &["checkout", "ahead"],
        &["merge", "ahead"],
        &["merge", "--no-ff", "ahead"],
        &["cherry-pick", "ahead"],
        &["revert", "--no-edit", "HEAD"],
        &["rebase", "ahead"],
        &["switch", "-c", "fresh", "ahead"],
        &["switch", "-C", "fresh", "ahead"],
        &["checkout", "-B", "fresh", "ahead"],
        &["reset", "--hard", "ahead"],
    ];
    for command in commands {
        let refused = alt(root, command);
        assert!(!refused.status.success(), "{command:?} went through");
        assert_eq!(told(root), before, "after {command:?}");
    }
    // nothing is left to finish: the next command runs, and with the file
    // out of the way so does the first of them
    std::fs::write(root.join("c"), "c\n").unwrap();
    ok(alt(root, &["add", "c"]));
    ok(alt(root, &["reset", "-q"]));
    std::fs::remove_file(root.join("c")).unwrap();
    std::fs::remove_file(root.join("a/u")).unwrap();
    ok(alt(root, &["switch", "ahead"]));
    assert_eq!(
        std::fs::read_to_string(root.join("a")).unwrap(),
        "file again\n"
    );
}

#[test]
fn an_undo_a_directory_is_in_the_way_of_takes_nothing_back() {
    let dir = directory_in_the_way();
    let root = dir.path();
    std::fs::remove_file(root.join("a/u")).unwrap();
    ok(alt(root, &["switch", "ahead"]));
    // the switch back puts `a/b` where the file was; its undo wants the file
    ok(alt(root, &["switch", "main"]));
    std::fs::write(root.join("a/u"), "u\n").unwrap();
    let before = told(root);
    let refused = alt(root, &["undo"]);
    assert!(!refused.status.success());
    assert_eq!(told(root), before);
    std::fs::remove_file(root.join("a/u")).unwrap();
    ok(alt(root, &["undo"]));
    assert_eq!(
        ok(alt(root, &["rev-parse", "--abbrev-ref", "HEAD"])),
        "ahead\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("a")).unwrap(),
        "file again\n"
    );
}
