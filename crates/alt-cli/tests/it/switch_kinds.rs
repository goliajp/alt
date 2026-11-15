//! `alt switch` between commits where a path is a directory on one side and
//! a file or symlink on the other.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

fn clean(root: &Path) -> bool {
    ok(alt(root, &["status"])).contains("nothing to commit")
}

/// Branch `tree` holds `node/` as a directory; branch `flat`, cut from it,
/// replaces the directory with whatever `flatten` puts at `node`.
fn two_kinds(root: &Path, flatten: &dyn Fn(&Path)) {
    ok(alt(root, &["init", "."]));
    write(root, "keep.txt", "k\n");
    write(root, "node/a.txt", "a\n");
    write(root, "node/deep/b.txt", "b\n");
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "node is a directory"]));
    ok(alt(root, &["branch", "tree"]));
    ok(alt(root, &["switch", "-c", "flat"]));
    std::fs::remove_dir_all(root.join("node")).unwrap();
    flatten(root);
    ok(alt(root, &["add", "."]));
    ok(alt(
        root,
        &["commit", "-m", "node is no longer a directory"],
    ));
    assert!(clean(root));
}

#[test]
fn switch_turns_a_file_into_a_directory_and_back() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    two_kinds(root, &|root| write(root, "node", "flat\n"));

    for _ in 0..2 {
        ok(alt(root, &["switch", "tree"]));
        assert_eq!(read(root, "node/a.txt"), "a\n");
        assert_eq!(read(root, "node/deep/b.txt"), "b\n");
        assert!(clean(root));

        ok(alt(root, &["switch", "flat"]));
        assert_eq!(read(root, "node"), "flat\n");
        assert_eq!(read(root, "keep.txt"), "k\n");
        assert!(clean(root));
    }

    // undo takes the last switch back, across the same change of kind
    ok(alt(root, &["undo"]));
    assert_eq!(read(root, "node/deep/b.txt"), "b\n");
    assert!(ok(alt(root, &["status"])).contains("On branch tree"));
    assert!(clean(root));
}

#[test]
fn switch_keeps_untracked_files_in_a_directory_that_would_become_a_file() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    two_kinds(root, &|root| write(root, "node", "flat\n"));
    ok(alt(root, &["switch", "tree"]));
    write(root, "node/deep/notes.txt", "not tracked\n");

    let refused = alt(root, &["switch", "flat"]);
    assert!(!refused.status.success());
    let message = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(message.contains("holding untracked files"), "{message}");
    assert_eq!(read(root, "node/deep/notes.txt"), "not tracked\n");
    assert_eq!(read(root, "node/a.txt"), "a\n");
    assert!(ok(alt(root, &["status"])).contains("On branch tree"));

    // an empty directory left behind is no obstacle
    std::fs::remove_file(root.join("node/deep/notes.txt")).unwrap();
    std::fs::create_dir_all(root.join("node/empty/nested")).unwrap();
    ok(alt(root, &["switch", "flat"]));
    assert_eq!(read(root, "node"), "flat\n");
}

#[cfg(unix)]
#[test]
fn switch_turns_a_symlink_into_a_directory_without_writing_through_it() {
    let outside = alt_os::temp::tempdir().unwrap();
    write(outside.path(), "a.txt", "outside\n");
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    let target = outside.path().to_owned();
    two_kinds(root, &|root| {
        std::os::unix::fs::symlink(&target, root.join("node")).unwrap();
    });

    ok(alt(root, &["switch", "tree"]));
    assert!(!root.join("node").is_symlink());
    assert_eq!(read(root, "node/a.txt"), "a\n");
    assert_eq!(read(outside.path(), "a.txt"), "outside\n");
    assert!(!outside.path().join("deep").exists());

    ok(alt(root, &["switch", "flat"]));
    assert!(root.join("node").is_symlink());
    assert_eq!(read(outside.path(), "a.txt"), "outside\n");
    assert!(clean(root));
}

#[test]
fn switch_removes_ignored_files_with_a_directory_that_becomes_a_file() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    write(root, ".gitignore", "*.log\n");
    two_kinds(root, &|root| write(root, "node", "flat\n"));
    ok(alt(root, &["switch", "tree"]));
    write(root, "node/deep/build.log", "ignored output\n");

    // as in git, ignored files do not hold the directory in place
    ok(alt(root, &["switch", "flat"]));
    assert_eq!(read(root, "node"), "flat\n");
    assert!(clean(root));

    // they were recorded with the switch, so taking it back restores them
    ok(alt(root, &["undo"]));
    assert_eq!(read(root, "node/deep/build.log"), "ignored output\n");
    assert_eq!(read(root, "node/a.txt"), "a\n");

    // next to an untracked file that is not ignored, nothing is removed
    write(root, "node/deep/notes.txt", "not tracked\n");
    assert!(!alt(root, &["switch", "flat"]).status.success());
    assert_eq!(read(root, "node/deep/build.log"), "ignored output\n");
    assert_eq!(read(root, "node/deep/notes.txt"), "not tracked\n");
}

#[cfg(unix)]
#[test]
fn switch_leaves_alone_the_files_both_branches_share() {
    use std::os::unix::fs::MetadataExt;
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    two_kinds(root, &|root| write(root, "node", "flat\n"));
    let inode = |rel: &str| std::fs::metadata(root.join(rel)).unwrap().ino();
    let before = inode("keep.txt");
    ok(alt(root, &["switch", "tree"]));
    ok(alt(root, &["switch", "flat"]));
    assert_eq!(inode("keep.txt"), before, "keep.txt was written again");
}
