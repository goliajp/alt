//! Which directories the daemon watches, and what becomes of its watches
//! when a tree is gone or a look at it fails.

use crate::scan_daemon::{agree, daemon_pid, output, run, served, watches, write};

#[cfg(target_os = "linux")]
#[test]
fn the_daemon_takes_no_watches_for_what_ignore_rules_hide() {
    let (dir, stop) = served();
    let root = dir.path();
    // the root, docs, src, src/deep
    assert_eq!(watches(stop.0), (1, 4));
    write(root, ".gitignore", "/target/\n");
    for unit in 0..40 {
        write(root, &format!("target/debug/unit{unit}/out.o"), "o\n");
    }
    let status = agree(root, "build output");
    assert!(!status.contains("target"), "{status}");
    assert_eq!(watches(stop.0), (1, 4));
    // shown once the rule is gone, and followed from then on
    write(root, ".gitignore", "/elsewhere/\n");
    assert!(agree(root, "rule gone").contains("\ttarget/\n"));
    assert_eq!(watches(stop.0), (1, 46));
    std::fs::remove_dir_all(root.join("target")).unwrap();
    let status = agree(root, "output removed");
    assert!(!status.contains("target"), "{status}");
    assert_eq!(watches(stop.0), (1, 4));
}

#[cfg(target_os = "linux")]
#[test]
fn a_removed_workspace_gives_its_watches_back() {
    let (dir, stop) = served();
    let root = dir.path();
    let side = alt_os::temp::tempdir().unwrap();
    let tree = side.path().join("tree");
    run(root, &["branch", "feat"], false);
    let at = tree.to_str().unwrap();
    run(root, &["workspace", "add", "side", at, "feat"], false);
    assert_eq!(
        run(&tree, &["status"], true),
        run(&tree, &["status"], false)
    );
    assert_eq!(watches(stop.0), (2, 8));
    run(root, &["workspace", "remove", "side"], false);
    agree(root, "after the removal");
    assert_eq!(watches(stop.0), (1, 4));
    assert_eq!(daemon_pid(root), stop.0);
}

#[test]
fn a_tree_is_watched_as_before_once_its_ignore_file_can_be_read_again() {
    let (dir, _stop) = served();
    let root = dir.path();
    write(root, "a.txt", "changed\n");
    agree(root, "changed");
    // the user's ignore file cannot be read: a directory is in its place,
    // which ends every command that sets up the ignore rules
    std::fs::create_dir_all(root.join(".alt/test-config/git/ignore")).unwrap();
    let ended: [&[&str]; 3] = [&["status"], &["status", "-uno"], &["diff", "--cached"]];
    for args in ended {
        for daemon in [true, false] {
            let said = output(root, args, daemon);
            let stderr = String::from_utf8_lossy(&said.stderr);
            assert_eq!(said.status.code(), Some(128), "{args:?} daemon: {daemon}");
            assert!(stderr.contains("git/ignore as an exclude file"), "{stderr}");
        }
    }
    write(root, "src/lib.rs", "changed as well\n");
    assert_eq!(run(root, &["diff"], true), run(root, &["diff"], false));
    assert!(run(root, &["diff"], true).contains("src/lib.rs"));
    std::fs::remove_dir(root.join(".alt/test-config/git/ignore")).unwrap();
    assert!(agree(root, "readable again").contains("modified:   src/lib.rs"));
}
