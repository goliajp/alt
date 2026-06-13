//! `alt branch` / `alt switch`: create and list branches, materialize the
//! target tree on switch (adding and removing files), refuse to clobber
//! uncommitted work, and export to a git repo git itself accepts.

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
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
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn branch_switch_materializes_tree_and_exports_clean() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));

    std::fs::write(root.join("a.txt"), "hello\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));

    // create + list: main is current, feat exists
    ok(alt(root, &["branch", "feat"]));
    let list = ok(alt(root, &["branch"]));
    assert!(list.contains("* main"), "current branch starred: {list}");
    assert!(list.contains("  feat"), "feat listed: {list}");

    // on feat, add a file that main does not have
    ok(alt(root, &["switch", "feat"]));
    std::fs::write(root.join("b.txt"), "world\n").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "second"]));
    assert!(root.join("b.txt").exists());

    // switch to main: b.txt must vanish, a.txt stays
    let sw = ok(alt(root, &["switch", "main"]));
    assert!(sw.contains("Switched to branch 'main'"), "{sw}");
    assert!(
        !root.join("b.txt").exists(),
        "feat-only file removed on main"
    );
    assert!(root.join("a.txt").exists(), "shared file kept");
    assert!(
        ok(alt(root, &["status"])).contains("working tree clean"),
        "main is clean after switch"
    );

    // back to feat: b.txt returns
    ok(alt(root, &["switch", "feat"]));
    assert!(root.join("b.txt").exists(), "feat file restored on switch");

    // protection: a staged change must block a switch
    std::fs::write(root.join("c.txt"), "dirty\n").unwrap();
    ok(alt(root, &["add", "."]));
    let blocked = alt(root, &["switch", "main"]);
    assert!(!blocked.status.success(), "dirty switch must fail");
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("local changes"),
        "stderr: {}",
        String::from_utf8_lossy(&blocked.stderr)
    );

    // -c creates and switches in one step, carrying the staged c.txt over
    let created = ok(alt(root, &["switch", "-c", "feat2"]));
    assert!(
        created.contains("Switched to a new branch 'feat2'"),
        "{created}"
    );
    assert!(ok(alt(root, &["branch"])).contains("* feat2"));
    ok(alt(root, &["commit", "-m", "third"])); // commit so the tree is clean

    // delete a non-current branch
    ok(alt(root, &["switch", "main"]));
    ok(alt(root, &["branch", "-d", "feat2"]));
    assert!(!ok(alt(root, &["branch"])).contains("feat2"));
    // deleting the current branch is refused
    assert!(!alt(root, &["branch", "-d", "main"]).status.success());

    // export and let git judge both branches
    let out = tempfile::tempdir().unwrap();
    let target = out.path().join("exported");
    ok(alt(root, &["export", target.to_str().unwrap()]));
    assert!(
        git(&target, &["fsck", "--strict"]).status.success(),
        "exported repo must be git-fsck clean"
    );
    let main_ls = ok(git(&target, &["ls-tree", "-r", "--name-only", "main"]));
    assert!(
        main_ls.contains("a.txt") && !main_ls.contains("b.txt"),
        "main tree: {main_ls}"
    );
    let feat_ls = ok(git(&target, &["ls-tree", "-r", "--name-only", "feat"]));
    assert!(
        feat_ls.contains("a.txt") && feat_ls.contains("b.txt"),
        "feat tree: {feat_ls}"
    );
}
