//! `alt mv` renames what it moves, as git does: a file keeps its
//! permissions, its times and its inode, and a directory goes whole,
//! with the files in it that are not tracked and its empty directories.

#[cfg(target_os = "linux")]
mod across_devices;
mod directories;
mod on_casefold;

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use crate::like_git::{Pair, alt, ok};

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

/// A tracked file and a private one that is not tracked in `d`, and an
/// empty directory.
fn with_directory() -> Pair {
    let pair = Pair::new();
    pair.write("d/t", "t\n");
    pair.write("top", "top\n");
    pair.commit_all("one");
    pair.write("d/secret", "s\n");
    for root in [&pair.alt, &pair.git] {
        let private = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(root.join("d/secret"), private).unwrap();
        std::fs::create_dir(root.join("d/empty")).unwrap();
        let odd = std::fs::Permissions::from_mode(0o640);
        std::fs::set_permissions(root.join("top"), odd).unwrap();
    }
    pair
}

#[test]
fn a_directory_moves_whole_and_comes_back_whole() {
    let pair = with_directory();
    let was = std::fs::symlink_metadata(pair.alt.join("d/t")).unwrap();
    let dir = std::fs::symlink_metadata(pair.alt.join("d")).unwrap().ino();
    pair.both(&["mv", "d", "e"]);
    pair.assert_same_state();
    for root in [&pair.alt, &pair.git] {
        assert_eq!(mode(&root.join("e/secret")), 0o600);
        assert!(root.join("e/empty").is_dir());
        assert!(!root.join("d").exists());
    }
    let now = std::fs::symlink_metadata(pair.alt.join("e/t")).unwrap();
    assert_eq!(
        (now.ino(), now.mtime(), now.mtime_nsec()),
        (was.ino(), was.mtime(), was.mtime_nsec())
    );
    assert_eq!(
        std::fs::symlink_metadata(pair.alt.join("e")).unwrap().ino(),
        dir
    );

    ok(alt(&pair.alt, &["undo"]));
    assert_eq!(mode(&pair.alt.join("d/secret")), 0o600);
    assert!(pair.alt.join("d/empty").is_dir() && !pair.alt.join("e").exists());
    let back = std::fs::symlink_metadata(pair.alt.join("d/t")).unwrap();
    assert_eq!(
        (back.ino(), back.mtime_nsec()),
        (was.ino(), was.mtime_nsec())
    );
}

#[test]
fn a_file_moves_with_its_permissions_and_its_times() {
    let pair = with_directory();
    let was = std::fs::symlink_metadata(pair.alt.join("top")).unwrap();
    pair.both(&["mv", "top", "d/top"]);
    pair.assert_same_state();
    for root in [&pair.alt, &pair.git] {
        assert_eq!(mode(&root.join("d/top")), 0o640);
    }
    let now = std::fs::symlink_metadata(pair.alt.join("d/top")).unwrap();
    assert_eq!(
        (now.ino(), now.mtime(), now.mtime_nsec()),
        (was.ino(), was.mtime(), was.mtime_nsec())
    );
    // a directory into a directory that is there
    std::fs::create_dir(pair.alt.join("into")).unwrap();
    std::fs::create_dir(pair.git.join("into")).unwrap();
    pair.both(&["mv", "d", "into"]);
    pair.assert_same_state();
    for root in [&pair.alt, &pair.git] {
        assert_eq!(mode(&root.join("into/d/secret")), 0o600);
        assert!(root.join("into/d/empty").is_dir() && !root.join("d").exists());
    }
}

#[test]
fn a_file_moved_over_another_keeps_its_permissions_and_its_times() {
    let pair = with_directory();
    pair.write("other", "o\n");
    pair.commit_all("two");
    for root in [&pair.alt, &pair.git] {
        let private = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(root.join("top"), private).unwrap();
    }
    let was = std::fs::symlink_metadata(pair.alt.join("top")).unwrap();
    pair.both(&["mv", "-f", "top", "other"]);
    pair.assert_same_state();
    for root in [&pair.alt, &pair.git] {
        assert_eq!(mode(&root.join("other")), 0o600);
        assert!(!root.join("top").exists());
    }
    let now = std::fs::symlink_metadata(pair.alt.join("other")).unwrap();
    assert_eq!(
        (now.ino(), now.mtime(), now.mtime_nsec()),
        (was.ino(), was.mtime(), was.mtime_nsec())
    );
    // taken back, each has what it held
    ok(alt(&pair.alt, &["undo"]));
    let read = |name: &str| std::fs::read_to_string(pair.alt.join(name)).unwrap();
    assert_eq!((read("top"), read("other")), ("top\n".into(), "o\n".into()));
}

/// A second link to a file is another name, not another spelling of
/// the one: git does not move the file to it unless forced, and then
/// the rename leaves both links where they are.
pub(crate) fn a_link_is_another_name(pair: &Pair) {
    pair.write("linked", "l\n");
    pair.commit_all("linked");
    for root in [&pair.alt, &pair.git] {
        std::fs::hard_link(root.join("linked"), root.join("second")).unwrap();
    }
    let refused = pair.same_failure(&["mv", "linked", "second"], 128);
    assert_eq!(
        refused,
        "fatal: destination exists, source=linked, destination=second\n"
    );
    pair.both(&["mv", "-f", "linked", "second"]);
    assert_eq!(
        crate::like_git::index(&pair.alt),
        ok(crate::like_git::git(&pair.git, &["ls-files", "-s"]))
    );
    for root in [&pair.alt, &pair.git] {
        assert!(root.join("linked").is_file() && root.join("second").is_file());
    }
    let said = ok(alt(&pair.alt, &["status"]));
    assert!(
        said.contains("new file:   second") && said.contains("Untracked files:\n\tlinked"),
        "{said}"
    );
}

#[test]
fn a_second_link_to_a_file_is_not_moved_to_unless_forced() {
    a_link_is_another_name(&Pair::new());
}
