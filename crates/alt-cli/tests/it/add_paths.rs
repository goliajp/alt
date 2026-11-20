//! `alt add <dir>` stages everything under the directory, deletions included,
//! and a commit that would change nothing is refused — both as in git.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    crate::alt_command()
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
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

fn tree_line(cat: &str) -> String {
    cat.lines().next().unwrap().to_owned()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// The same steps for either tool: `run` executes one of its commands.
fn scenario(root: &Path, run: &dyn Fn(&[&str]) -> Output) {
    write(root, "top.txt", "t\n");
    write(root, "src/a.rs", "a\n");
    write(root, "src/deep/b.rs", "b\n");
    write(root, "other/c.txt", "c\n");
    ok(run(&["add", "."]));
    ok(run(&["commit", "-m", "base"]));
    // change and delete under src/, touch other/ and a look-alike too
    write(root, "src/a.rs", "a2\n");
    std::fs::remove_file(root.join("src/deep/b.rs")).unwrap();
    write(root, "src/new.rs", "n\n");
    write(root, "other/c.txt", "c2\n");
    write(root, "srcish.txt", "not under src\n");
    ok(run(&["add", "./src/"]));
    ok(run(&["commit", "-m", "src only"]));
}

#[test]
fn adding_a_directory_matches_git() {
    let (a, g) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (a, g) = (a.path(), g.path());
    ok(alt(a, &["init", "."]));
    ok(git(g, &["init", "-q", "-b", "main"]));
    scenario(a, &|args| alt(a, args));
    scenario(g, &|args| git(g, args));
    let alt_head = ok(alt(a, &["cat-file", "-p", "main"]));
    let git_head = ok(git(g, &["cat-file", "-p", "HEAD"]));
    assert_eq!(tree_line(&alt_head), tree_line(&git_head));
    // other/ and srcish.txt were left unstaged
    let st = ok(alt(a, &["status"]));
    assert!(
        st.contains("other/c.txt") && st.contains("srcish.txt"),
        "{st}"
    );
}

#[test]
fn a_path_matching_nothing_is_an_error() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    write(root, "a.txt", "a\n");
    let o = alt(root, &["add", "nope"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("pathspec 'nope' did not match any files"));
    assert!(!ok(alt(root, &["op-log"])).contains("verb=add"));
}

#[test]
fn a_commit_that_changes_nothing_is_refused_unless_asked_for() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    write(root, "a.txt", "a\n");
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));
    let before = ok(alt(root, &["rev-parse", "main"]));

    let o = alt(root, &["commit", "-m", "nothing"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("nothing to commit"));
    assert_eq!(ok(alt(root, &["rev-parse", "main"])), before);

    ok(alt(root, &["commit", "--allow-empty", "-m", "on purpose"]));
    assert_ne!(ok(alt(root, &["rev-parse", "main"])), before);
}

#[test]
fn add_keeps_a_submodule_entry() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    ok(git(src, &["init", "-q", "-b", "main", "."]));
    write(src, "a.txt", "one\n");
    write(src, "dir/b.txt", "two\n");
    ok(git(src, &["add", "."]));
    ok(git(
        src,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000,1111111111111111111111111111111111111111,sub",
        ],
    ));
    ok(git(src, &["commit", "-qm", "base"]));
    ok(alt(src, &["import", dst.to_str().unwrap()]));
    std::fs::create_dir(src.join("sub")).unwrap();

    // the whole tree, then the submodule path by name
    for (round, spec) in [".", "sub"].into_iter().enumerate() {
        for root in [src, dst] {
            write(root, "a.txt", &format!("edit {round}\n"));
        }
        ok(alt(dst, &["add", spec]));
        ok(alt(dst, &["add", "a.txt"]));
        ok(alt(dst, &["commit", "-m", "edit"]));
        ok(git(src, &["add", "."]));
        ok(git(src, &["commit", "-qm", "edit"]));
        assert_eq!(
            tree_line(&ok(alt(dst, &["cat-file", "-p", "HEAD"]))),
            tree_line(&ok(git(src, &["cat-file", "-p", "HEAD"]))),
            "after add {spec}"
        );
    }
    assert!(ok(alt(dst, &["status"])).contains("nothing to commit"));
}

#[test]
fn add_stages_a_file_replaced_by_a_directory() {
    let (ours, theirs) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    ok(alt(ours.path(), &["init", "."]));
    ok(git(theirs.path(), &["init", "-q", "."]));
    for (root, run) in [
        (ours.path(), &alt as &dyn Fn(&Path, &[&str]) -> Output),
        (theirs.path(), &git),
    ] {
        write(root, "node", "a file\n");
        write(root, "keep.txt", "k\n");
        ok(run(root, &["add", "."]));
        ok(run(root, &["commit", "-m", "base"]));
        std::fs::remove_file(root.join("node")).unwrap();
        write(root, "node/inner.txt", "now a directory\n");
        ok(run(root, &["add", "."]));
        ok(run(root, &["commit", "-m", "file becomes directory"]));
    }
    assert_eq!(
        tree_line(&ok(alt(ours.path(), &["cat-file", "-p", "HEAD"]))),
        tree_line(&ok(git(theirs.path(), &["cat-file", "-p", "HEAD"])))
    );
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        match entry.file_type().unwrap().is_dir() {
            true => copy_tree(&entry.path(), &target),
            false => drop(std::fs::copy(entry.path(), target).unwrap()),
        }
    }
}

/// A git repository with a file and a submodule entry at `sub`, and its
/// alt import.
fn with_submodule(src: &Path, dst: &Path) {
    ok(git(src, &["init", "-q", "-b", "main", "."]));
    write(src, "a.txt", "one\n");
    ok(git(src, &["add", "."]));
    ok(git(
        src,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000,1111111111111111111111111111111111111111,sub",
        ],
    ));
    ok(git(src, &["commit", "-qm", "base"]));
    ok(alt(src, &["import", dst.to_str().unwrap()]));
}

#[test]
fn add_leaves_files_in_a_submodule_directory_to_the_submodule() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    with_submodule(src, dst);
    // files in the directory, but no repository: neither tool takes them
    for root in [src, dst] {
        write(root, "sub/inner.txt", "belongs to the submodule\n");
        write(root, "sub/deep/more.txt", "so does this\n");
        write(root, "a.txt", "two\n");
    }
    for spec in [".", "sub"] {
        ok(alt(dst, &["add", spec]));
    }
    ok(alt(dst, &["commit", "-m", "edit"]));
    ok(git(src, &["add", "."]));
    ok(git(src, &["commit", "-qm", "edit"]));
    assert_eq!(
        tree_line(&ok(alt(dst, &["cat-file", "-p", "HEAD"]))),
        tree_line(&ok(git(src, &["cat-file", "-p", "HEAD"])))
    );
    assert!(ok(alt(dst, &["status"])).contains("nothing to commit"));

    let inside = alt(dst, &["add", "sub/inner.txt"]);
    assert!(!inside.status.success());
    let message = String::from_utf8_lossy(&inside.stderr).into_owned();
    assert!(message.contains("is in submodule 'sub'"), "{message}");
}

#[test]
fn a_submodule_that_checked_out_another_commit_is_reported_and_staged() {
    let (src, dst) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    let (src, dst) = (src.path(), dst.path());
    with_submodule(src, dst);
    // the same repository in both submodule directories
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    ok(git(&sub, &["init", "-q", "-b", "main", "."]));
    write(&sub, "inner.txt", "content of the submodule\n");
    ok(git(&sub, &["add", "."]));
    ok(git(&sub, &["commit", "-qm", "in the submodule"]));
    let head = ok(git(&sub, &["rev-parse", "HEAD"]));
    copy_tree(&sub, &dst.join("sub"));

    let status = ok(alt(dst, &["status"]));
    assert!(status.contains("modified:   sub"), "{status}");
    let diff = ok(alt(dst, &["diff"]));
    assert!(
        diff.contains(&format!("+Subproject commit {}", head.trim())),
        "{diff}"
    );
    assert!(!diff.contains("inner.txt"), "{diff}");
    // it does not stand in the way of a switch, which leaves it alone
    ok(alt(dst, &["branch", "other"]));
    ok(alt(dst, &["switch", "other"]));
    ok(alt(dst, &["switch", "main"]));
    assert!(ok(alt(dst, &["status"])).contains("modified:   sub"));

    ok(alt(dst, &["add", "."]));
    ok(alt(dst, &["commit", "-m", "move the submodule"]));
    ok(git(src, &["add", "."]));
    ok(git(src, &["commit", "-qm", "move the submodule"]));
    assert_eq!(
        tree_line(&ok(alt(dst, &["cat-file", "-p", "HEAD"]))),
        tree_line(&ok(git(src, &["cat-file", "-p", "HEAD"])))
    );
    assert!(ok(alt(dst, &["status"])).contains("nothing to commit"));
}

#[test]
fn a_file_under_a_directory_that_became_a_file_can_be_staged_by_name() {
    let (ours, theirs) = (
        alt_os::temp::tempdir().unwrap(),
        alt_os::temp::tempdir().unwrap(),
    );
    ok(alt(ours.path(), &["init", "."]));
    ok(git(theirs.path(), &["init", "-q", "."]));
    for (root, run) in [
        (ours.path(), &alt as &dyn Fn(&Path, &[&str]) -> Output),
        (theirs.path(), &git),
    ] {
        write(root, "node/a.txt", "a\n");
        write(root, "node/b.txt", "b\n");
        write(root, "other/c.txt", "c\n");
        ok(run(root, &["add", "."]));
        ok(run(root, &["commit", "-m", "base"]));
        std::fs::remove_dir_all(root.join("node")).unwrap();
        write(root, "node", "now a file\n");
        // only the one path: its removal is staged, the rest is not
        ok(run(root, &["add", "node/a.txt"]));
        ok(run(root, &["commit", "-m", "drop one file"]));

        // through a symlink a path cannot be named, in either tool
        std::fs::remove_dir_all(root.join("other")).unwrap();
        std::os::unix::fs::symlink("node", root.join("other")).unwrap();
        let refused = run(root, &["add", "other/c.txt"]);
        assert!(!refused.status.success());
        let message = String::from_utf8_lossy(&refused.stderr).into_owned();
        assert!(message.contains("is beyond a symbolic link"), "{message}");
    }
    assert_eq!(
        tree_line(&ok(alt(ours.path(), &["cat-file", "-p", "HEAD"]))),
        tree_line(&ok(git(theirs.path(), &["cat-file", "-p", "HEAD"])))
    );
    let status = ok(alt(ours.path(), &["status"]));
    assert!(status.contains("deleted:    node/b.txt"), "{status}");
}
