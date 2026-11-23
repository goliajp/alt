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
