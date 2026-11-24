//! A move to another file system: git renames, a rename does not go
//! from one file system to another, and so nothing is moved.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::like_git::{Pair, git, index, ok};

/// A file system of its own at a directory, until this is dropped.
struct Mounted(PathBuf);

impl Mounted {
    fn at(dir: &Path) -> Self {
        let mounted = Command::new("mount")
            .args(["-t", "tmpfs", "none"])
            .arg(dir)
            .status()
            .unwrap();
        assert!(mounted.success(), "mount at {}", dir.display());
        Mounted(dir.to_owned())
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        let _ = Command::new("umount").arg(&self.0).status();
    }
}

#[test]
#[ignore = "needs root: mounts a file system inside the working tree"]
fn nothing_is_moved_to_another_file_system() {
    let pair = Pair::new();
    pair.write("a", "a\n");
    pair.write("d/x", "x\n");
    pair.write("other/keep", "k\n");
    pair.commit_all("one");
    let _mounted = [&pair.alt, &pair.git].map(|root| Mounted::at(&root.join("other")));
    for (moved, from) in [
        (vec!["mv", "a", "other/a"], "a"),
        (vec!["mv", "-f", "a", "other/a"], "a"),
        (vec!["mv", "d", "other/d"], "d"),
        (vec!["mv", "a", "d", "other"], "a"),
    ] {
        let said = pair.same_failure(&moved, 128);
        let crossed = format!("fatal: renaming '{from}' failed: Invalid cross-device link\n");
        assert_eq!(said, crossed, "{moved:?}");
        assert_eq!(index(&pair.alt), ok(git(&pair.git, &["ls-files", "-s"])));
        for root in [&pair.alt, &pair.git] {
            assert!(root.join("a").is_file() && root.join("d/x").is_file());
            assert!(
                std::fs::read_dir(root.join("other"))
                    .unwrap()
                    .next()
                    .is_none()
            );
        }
    }
}
