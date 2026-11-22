//! The snapshots a command that syncs makes durable of those the adds
//! before it left unsynced: the two of the last add, however many adds
//! there were, and what an undo and a later add make of the others.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use super::staged_durability::{add, repo};
use super::synced_files::{library, synced};
use super::unconfirmed::{alt, alt_command, ok};

const SNAPSHOTS: &str = ".alt/recovery/objects";

fn snapshots(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir.join(SNAPSHOTS))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Adds `names` one by one, each left unsynced; the snapshot each add
/// wrote, in that order.
fn added(dir: &Path, names: &[&str]) -> Vec<String> {
    let mut wrote = Vec::new();
    for name in names {
        let before = snapshots(dir);
        add(dir, name);
        let new: Vec<_> = snapshots(dir)
            .into_iter()
            .filter(|name| !before.contains(name))
            .collect();
        assert_eq!(new.len(), 1, "{name}");
        wrote.extend(new);
    }
    wrote
}

/// What a commit after adds of `count` files syncs, with the names that
/// differ from one run to the next made alike.
fn commit_syncs(root: &Path, count: usize) -> (Vec<String>, Vec<String>, Vec<String>) {
    let dir = &root.join(format!("repo{count}"));
    fs::create_dir(dir).unwrap();
    repo(dir);
    let names: Vec<_> = (0..count).map(|n| format!("f{n}.txt")).collect();
    let names: Vec<_> = names.iter().map(String::as_str).collect();
    let wrote = added(dir, &names);
    let library = library(root);
    let (out, synced) = synced(dir, &library, alt_command(dir, &["commit", "-m", "second"]));
    ok(out);
    let alike = |path: &String| match path {
        path if path.starts_with(".alt/requests/") => ".alt/requests/*".to_owned(),
        path if path.starts_with(SNAPSHOTS) && path.len() > SNAPSHOTS.len() => "snapshot".into(),
        path => path.clone(),
    };
    let index = fs::metadata(dir.join(".alt/index")).unwrap().ino();
    let last = fs::metadata(dir.join(SNAPSHOTS).join(wrote.last().unwrap())).unwrap();
    assert_eq!(
        last.ino(),
        index,
        "the index is its snapshot under another name"
    );
    (synced.iter().map(alike).collect(), synced, wrote)
}

#[test]
fn a_commit_syncs_as_many_files_after_many_adds_as_after_two() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let (two, ..) = commit_syncs(tmp.path(), 2);
    let (many, synced, wrote) = commit_syncs(tmp.path(), 14);
    assert_eq!(two, many);
    // of the snapshots the adds wrote: the one the last add began from,
    // and the one it left, which is the index
    let by_name = |name: &String| synced.contains(&format!("{SNAPSHOTS}/{name}"));
    let (last, earlier) = wrote.split_last().unwrap();
    let (before_last, earlier) = earlier.split_last().unwrap();
    assert!(by_name(before_last), "{synced:?}");
    assert!(synced.contains(&".alt/index".to_owned()), "{synced:?}");
    assert!(!by_name(last), "the same file as the index: {synced:?}");
    for name in earlier {
        assert!(!by_name(name), "{name} of {synced:?}");
    }
    // the pack and the map with what the adds appended and then with the
    // commit and its note; the two files that did not grow not at all
    let times = |path: &str| many.iter().filter(|synced| *synced == path).count();
    for path in [".alt/store/pack-00000001.altpack", ".alt/map.alt"] {
        assert_eq!(times(path), 2, "{path}: {synced:?}");
    }
    for path in [".alt/store/manifests/blobmap", ".alt/tier1"] {
        assert_eq!(times(path), 0, "{path}: {synced:?}");
    }
    assert_eq!(times("snapshot"), 1, "{synced:?}");
}

/// What a power cut makes of a file that was written and never synced
/// while the directory it is in was: its name with nothing in it.
fn cut(dir: &Path, snapshot: &str) {
    fs::write(dir.join(SNAPSHOTS).join(snapshot), b"").unwrap();
}

#[test]
fn the_last_add_is_undone_after_a_power_cut_took_the_snapshots_of_those_before_it() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let wrote = added(dir, &["b.txt", "c.txt", "d.txt"]);
    // a command that syncs, which is not the last operation's own
    ok(alt(dir, &["config", "core.fsync", "added"]));
    add(dir, "e.txt");
    // what the adds before the last one left is not read again
    cut(dir, &wrote[0]);
    cut(dir, &wrote[1]);
    ok(alt(dir, &["undo"]));
    let status = ok(alt(dir, &["status"]));
    assert!(status.contains("new file:   d.txt"), "{status}");
    assert!(!status.contains("new file:   e.txt"), "{status}");
    ok(alt(dir, &["undo"]));
    let status = ok(alt(dir, &["status"]));
    assert!(status.contains("new file:   e.txt"), "{status}");
    ok(alt(dir, &["commit", "-m", "second"]));
    assert!(ok(alt(dir, &["fsck"])).ends_with("every record verified\n"));
}

#[test]
fn a_snapshot_a_power_cut_emptied_is_written_anew_by_the_add_that_makes_its_index_again() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    repo(dir);
    let wrote = added(dir, &["b.txt", "c.txt", "d.txt"]);
    ok(alt(dir, &["commit", "-m", "second"]));
    // the commit synced the directory and not the index after `b.txt`
    cut(dir, &wrote[0]);
    ok(alt(dir, &["status"]));
    assert!(ok(alt(dir, &["fsck"])).ends_with("every record verified\n"));
    // with the two files after it gone the index is that one again
    fs::remove_file(dir.join("c.txt")).unwrap();
    fs::remove_file(dir.join("d.txt")).unwrap();
    let before = snapshots(dir);
    ok(alt(dir, &["add", "c.txt", "d.txt"]));
    assert_eq!(snapshots(dir), before, "no other snapshot was written");
    let kept = fs::read(dir.join(SNAPSHOTS).join(&wrote[0])).unwrap();
    assert_eq!(alt_hash::blake3::hash(&kept).to_hex().to_string(), wrote[0]);
    let staged = "Changes to be committed";
    assert!(ok(alt(dir, &["status"])).contains(staged));
    ok(alt(dir, &["undo"]));
    assert!(!ok(alt(dir, &["status"])).contains(staged));
    ok(alt(dir, &["undo"]));
    assert!(ok(alt(dir, &["status"])).contains(staged));
    ok(alt(dir, &["commit", "-m", "third"]));
    assert!(ok(alt(dir, &["fsck"])).ends_with("every record verified\n"));
}
