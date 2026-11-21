//! The index an index change puts in place and the recovery snapshot of
//! that index are one file under two names: the bytes are written once,
//! and an add that syncs syncs them once.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use super::staged_durability::repo;
use super::unconfirmed::{alt, ok};

const INDEX: &str = ".alt/index";
const SNAPSHOTS: &str = ".alt/recovery/objects";

/// The recovery snapshots that are the very file the index is.
fn snapshots_of_the_index(dir: &Path) -> Vec<String> {
    let index = fs::metadata(dir.join(INDEX)).unwrap().ino();
    let listing = fs::read_dir(dir.join(SNAPSHOTS)).unwrap();
    let same = listing.filter_map(|entry| {
        let entry = entry.unwrap();
        let name = entry.file_name().into_string().unwrap();
        (entry.metadata().unwrap().ino() == index).then_some(name)
    });
    same.collect()
}

#[test]
fn an_index_change_leaves_the_index_and_its_snapshot_as_one_file() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let mut seen = Vec::new();
    for name in ["b.txt", "c.txt"] {
        fs::write(dir.join(name), name).unwrap();
        ok(alt(dir, &["add", name]));
        let same = snapshots_of_the_index(dir);
        assert_eq!(same.len(), 1, "{same:?}");
        assert_eq!(fs::metadata(dir.join(INDEX)).unwrap().nlink(), 2);
        seen.extend(same);
    }
    // an undo puts the snapshot from before the add in place the same way
    ok(alt(dir, &["undo"]));
    assert_eq!(snapshots_of_the_index(dir), seen[..1]);
    let status = ok(alt(dir, &["status"]));
    assert!(status.contains("new file:   b.txt"), "{status}");
    assert!(status.contains("Untracked files:"), "{status}");
    ok(alt(dir, &["commit", "-m", "second"]));
    assert!(ok(alt(dir, &["fsck"])).ends_with("every record verified\n"));
}

#[cfg(target_os = "linux")]
mod synced {
    use super::super::synced_files::{library, synced};
    use super::super::unconfirmed::alt_command;
    use super::*;

    /// `alt add b.txt` in `dir` as an add that syncs, and what it synced.
    fn add_synced(tmp: &Path, dir: &Path) -> Vec<String> {
        let library = library(tmp);
        let user = tmp.join("gitconfig");
        fs::write(&user, "[core]\n\tfsync = added\n").unwrap();
        fs::write(dir.join("b.txt"), "b\n").unwrap();
        let mut command = alt_command(dir, &["add", "b.txt"]);
        command.env("GIT_CONFIG_GLOBAL", &user);
        let (out, synced) = synced(dir, &library, command);
        ok(out);
        synced
    }

    fn beneath<'a>(synced: &'a [String], directory: &str) -> Vec<&'a str> {
        let inside = |path: &&str| Path::new(path).parent() == Some(Path::new(directory));
        synced.iter().map(String::as_str).filter(inside).collect()
    }

    #[test]
    fn an_add_that_syncs_syncs_the_index_as_its_snapshot_and_not_a_second_time() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = &tmp.path().join("repo");
        fs::create_dir(dir).unwrap();
        repo(dir);
        let synced = add_synced(tmp.path(), dir);
        // the snapshot, under the name it is written by; the one from
        // before the add is the index that was there, synced already
        let snapshots = beneath(&synced, SNAPSHOTS);
        assert_eq!(snapshots.len(), 1, "{synced:?}");
        assert!(snapshots[0].contains("/.tmp"), "{synced:?}");
        // no file is written beside the index to be renamed over it
        let beside = beneath(&synced, ".alt");
        assert!(
            !beside.iter().any(|path| path.contains("/.tmp")),
            "{synced:?}"
        );
        // the entry of the index is synced as it was
        assert!(synced.iter().any(|path| path == ".alt"), "{synced:?}");
        assert_eq!(snapshots_of_the_index(dir).len(), 1);
    }

    #[test]
    fn a_snapshot_that_was_there_is_synced_before_an_add_that_syncs_makes_it_the_index() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = &tmp.path().join("repo");
        fs::create_dir(dir).unwrap();
        repo(dir);
        // the same add in a copy of the repository tells which snapshot
        // the add writes; here it is put there by a plain write, unsynced
        let copy = &tmp.path().join("copy");
        let copied = std::process::Command::new("cp")
            .arg("-r")
            .args([dir, copy])
            .status();
        assert!(copied.unwrap().success());
        fs::write(copy.join("b.txt"), "b\n").unwrap();
        ok(alt(copy, &["add", "b.txt"]));
        let name = snapshots_of_the_index(copy).remove(0);
        let snapshot = Path::new(SNAPSHOTS).join(&name);
        fs::write(dir.join(&snapshot), fs::read(copy.join(INDEX)).unwrap()).unwrap();

        let synced = add_synced(tmp.path(), dir);
        assert_eq!(beneath(&synced, SNAPSHOTS), [snapshot.to_str().unwrap()]);
        assert_eq!(snapshots_of_the_index(dir), [name]);
    }
}
