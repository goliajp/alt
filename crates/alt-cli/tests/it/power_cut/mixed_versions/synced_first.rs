//! A removal of lost records writes the last records of a log over the
//! ones that go. Where those last records were left unsynced by an add
//! of the running boot, they come to lie among synced ones, which nobody
//! looks at again: the files are synced, content first, before anything
//! of the removal is written.

use std::fs;
use std::os::unix::fs::FileExt;

use super::super::super::mixed_versions::{new_bin, old_bin, run};
use super::super::super::staged_durability::repo;
use super::super::super::sync_order::FILES;
use super::super::super::synced_files::{library, synced};
use super::super::super::unconfirmed::alt_command;
use super::{an_older_build_wrote_past_a_lost_blob, ok};

#[test]
#[ignore = "run with ALT_OLD_BIN set to an alt built from the base branch"]
fn a_removal_syncs_the_files_before_it_moves_records_an_add_left_unsynced() {
    let (old, new) = (old_bin(), new_bin());
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = &tmp.path().join("repo");
    fs::create_dir(dir).unwrap();
    repo(dir);
    an_older_build_wrote_past_a_lost_blob(&old, dir);
    // without the note of the other boot this build does not look behind
    // what the older one recorded, and its add goes through, unsynced
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(dir.join(".alt/odb.lock"));
    lock.unwrap().write_all_at(&[0; 64], 1224).unwrap();
    fs::write(dir.join("d.txt"), "d\n").unwrap();
    ok(run(&new, dir, &["add", "d.txt"]));
    let library = library(tmp.path());
    let command = alt_command(dir, &["fsck", "--drop-lost", "--yes"]);
    let (taken, files) = synced(dir, &library, command);
    assert!(
        taken.status.success(),
        "{}",
        String::from_utf8_lossy(&taken.stderr)
    );
    let first = |name: &str| files.iter().position(|file| file.ends_with(name));
    let kept = first("odb.repair.tmp").unwrap_or_else(|| panic!("{files:?}"));
    let (pack, map) = (first(FILES[0]), first("map.alt"));
    assert!(pack < map && map < Some(kept), "{files:?}");
    assert!(pack.is_some(), "{files:?}");
    // the command holds a store it only reads, and the removal is made
    // beside it: the log the add left unsynced is not synced through it
    assert!(
        !files.iter().any(|file| file.contains("oplog")),
        "{files:?}"
    );
    // the record of d.txt lies where the lost one was, and reads
    ok(run(&new, dir, &["add", "b.txt"]));
    ok(run(&new, dir, &["commit", "-m", "second"]));
    assert!(ok(run(&new, dir, &["fsck"])).ends_with("every record verified\n"));
    assert_eq!(ok(run(&new, dir, &["show", "HEAD:d.txt"])), "d\n");
}
