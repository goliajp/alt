//! A FIFO, a socket or a device in the working tree is no file a tree
//! can hold: it is not listed as untracked and not added, by a
//! directory given or by its own name, as with git.

use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;

use crate::like_git::{Pair, alt, git, index, ok};

fn fifo(at: &Path) {
    let made = Command::new("mkfifo").arg(at).status().unwrap();
    assert!(made.success());
}

#[test]
fn what_is_no_file_or_directory_is_neither_listed_nor_added_as_by_git() {
    let pair = Pair::new();
    pair.write("a", "a\n");
    pair.commit_all("base");
    pair.write("new", "new\n");
    let mut held = Vec::new();
    for root in [&pair.alt, &pair.git] {
        fifo(&root.join("pipe"));
        std::fs::create_dir(root.join("sub")).unwrap();
        fifo(&root.join("sub/inner"));
        held.push(UnixListener::bind(root.join("sock")).unwrap());
    }
    let same_index = || assert_eq!(index(&pair.alt), ok(git(&pair.git, &["ls-files", "-s"])));
    let listed = ok(alt(&pair.alt, &["status"]));
    assert!(listed.contains("new"), "{listed}");
    for name in ["pipe", "sub", "sock"] {
        assert!(!listed.contains(name), "{listed}");
    }
    let theirs = ok(git(&pair.git, &["status", "--porcelain", "-uall"]));
    assert_eq!(theirs, "?? new\n");
    // by its own name git says nothing and adds nothing
    for named in [&["pipe"][..], &["sock"], &["sub"], &["sub/inner", "pipe"]] {
        let args = [&["add"], named].concat();
        let (ours, theirs) = pair.run(&args);
        assert_eq!(theirs.status.code(), Some(0), "{args:?}");
        assert_eq!(ours.status.code(), Some(0), "{args:?}");
        assert_eq!(ours.stderr, theirs.stderr, "{args:?}");
        same_index();
    }
    pair.both(&["add", "."]);
    same_index();
    pair.both(&["add", "-A"]);
    same_index();
    let listed = ok(alt(&pair.alt, &["status"]));
    assert!(
        !listed.contains("pipe") && !listed.contains("sock"),
        "{listed}"
    );
}
