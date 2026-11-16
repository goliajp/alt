//! Records that are whole on disk while the content they name is not: what
//! a power cut leaves when the kernel wrote out the pages of `map.alt`, the
//! Tier 1 map or the blob map before those of the file below. A writer
//! cuts such records when they lie past the synced boundaries, reports the
//! store when they do not, and never takes them at their word.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::time::SystemTime;

use alt_git_codec::ObjectKind;
use alt_odb::{NativeOdb, OdbError};

use super::marker::rewrite;
use super::rewind_interrupted::{
    ALL, FILES, KEPT, cut, lens, object, older_marker, open, put, reads,
};

/// The file whose end is lost, and which kind of object the lost content
/// belongs to (see `object`): the pack holds content of all three, the
/// blob map names the chunks of a blob of several, the Tier 1 map the
/// parts of a decomposed one.
const CASES: [(usize, u32); 5] = [(0, 0), (0, 1), (0, 2), (1, 1), (2, 2)];

/// The marker's flag for appends that were not synced.
const UNSYNCED_APPENDS: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Fault {
    /// the file ends where it did before the batch
    Missing,
    /// the last record is cut short
    Torn,
    /// the last record is whole in length and holds zeros at its end
    Zeroed,
}

/// A store of `KEPT` synced objects and then one more of `class`; the
/// file lengths before and after that one.
fn store(dir: &Path, class: u32) -> (u32, [u64; 4], [u64; 4]) {
    let victim = ALL + class;
    let mut odb = open(dir);
    put(&mut odb, 0..KEPT);
    let kept = lens(dir);
    put(&mut odb, victim..victim + 1);
    (victim, kept, lens(dir))
}

/// Makes the marker say what it would had the last batch not been synced
/// as far as `whole`, and takes back the note of how far the pack was:
/// it is written after the marker and never says more.
fn synced_to(dir: &Path, whole: [u64; 4], flags: u32) {
    rewrite(dir, |marker| {
        marker.flags = flags;
        marker.eof = whole;
        marker.whole = whole;
    });
    overwrite_lock(dir, 1176, 24);
}

/// Zeros `len` bytes of the lock file from `at` on: the head record
/// starts the file, the note of how far the pack is synced is at 1176.
fn overwrite_lock(dir: &Path, at: u64, len: usize) {
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(dir.join("odb.lock"))
        .unwrap();
    lock.write_all_at(&vec![0; len], at).unwrap();
}

fn damage(dir: &Path, file: usize, fault: Fault, kept: [u64; 4]) {
    let path = dir.join(FILES[file]);
    let mut data = fs::read(&path).unwrap();
    let len = data.len();
    assert!(len as u64 > kept[file] + 16, "{}", FILES[file]);
    match fault {
        Fault::Missing => cut(dir, file, kept[file]),
        Fault::Torn => cut(dir, file, len as u64 - 7),
        Fault::Zeroed => {
            data[len - 16..].fill(0);
            fs::write(&path, data).unwrap();
        }
    }
}

fn snapshot(dir: &Path) -> Vec<(u64, SystemTime)> {
    FILES
        .iter()
        .map(|name| {
            let meta = fs::metadata(dir.join(name)).unwrap();
            (meta.len(), meta.modified().unwrap())
        })
        .collect()
}

/// A put through a fresh handle: the first thing that takes the write lock.
fn try_put(dir: &Path, n: u32) -> Result<NativeOdb, OdbError> {
    let mut odb = open(dir);
    let (oid, data) = object(n);
    odb.put(oid, ObjectKind::Blob, &data)?;
    odb.flush()?;
    Ok(odb)
}

/// A handle that only reads finds the synced objects, never other bytes
/// for the one whose content is lost, and changes no file.
fn reader_is_sound(dir: &Path, victim: u32, case: &str) {
    let before = snapshot(dir);
    let odb = open(dir);
    for n in 0..KEPT {
        reads(&odb, n);
    }
    let (oid, data) = object(victim);
    if let Ok(Some(read)) = odb.get(&oid) {
        assert_eq!(read.data, data, "{case}");
    }
    drop(odb);
    assert_eq!(snapshot(dir), before, "{case}");
}

/// The store holds the synced objects and not the lost one, takes it
/// again, and passes the full check after a reopen.
fn recovered(dir: &Path, victim: u32, case: &str) {
    let mut odb = try_put(dir, victim + 3).unwrap_or_else(|e| panic!("{case}: {e:?}"));
    assert!(!odb.contains(&object(victim).0).unwrap(), "{case}");
    put(&mut odb, victim..victim + 1);
    reads(&odb, victim);
    drop(odb);
    let odb = open(dir);
    for n in (0..KEPT).chain([victim, victim + 3]) {
        reads(&odb, n);
    }
    odb.check().unwrap_or_else(|e| panic!("{case}: {e:?}"));
}

#[test]
fn unsynced_records_naming_content_that_never_arrived_are_cut_with_it() {
    for (file, class) in CASES {
        for fault in [Fault::Missing, Fault::Torn, Fault::Zeroed] {
            let case = format!("{} {fault:?} class {class}", FILES[file]);
            let tmp = alt_os::temp::tempdir().unwrap();
            let (victim, kept, _) = store(tmp.path(), class);
            // the marker as it was before the batch: none of it was synced
            synced_to(tmp.path(), kept, 0);
            damage(tmp.path(), file, fault, kept);
            reader_is_sound(tmp.path(), victim, &case);
            recovered(tmp.path(), victim, &case);
        }
    }
}

#[test]
fn a_synced_record_naming_content_that_is_cut_is_damage_and_no_file_changes() {
    for (file, class) in CASES {
        for fault in [Fault::Torn, Fault::Zeroed] {
            let case = format!("{} {fault:?} class {class}", FILES[file]);
            let tmp = alt_os::temp::tempdir().unwrap();
            let (victim, kept, full) = store(tmp.path(), class);
            // synced: every file that names the content, not the content
            let mut whole = full;
            whole[file] = kept[file];
            synced_to(tmp.path(), whole, 0);
            damage(tmp.path(), file, fault, kept);
            reader_is_sound(tmp.path(), victim, &case);
            let before = snapshot(tmp.path());
            let refused = try_put(tmp.path(), victim + 3).err();
            assert!(
                matches!(refused, Some(OdbError::Dangling { .. })),
                "{case}: {refused:?}"
            );
            assert_eq!(snapshot(tmp.path()), before, "{case}");
        }
    }
}

#[test]
fn with_no_boundaries_a_cut_that_would_leave_a_record_naming_nothing_is_refused() {
    for (file, class) in CASES {
        for fault in [Fault::Torn, Fault::Zeroed] {
            let case = format!("{} {fault:?} class {class}", FILES[file]);
            let tmp = alt_os::temp::tempdir().unwrap();
            let (victim, kept, _) = store(tmp.path(), class);
            // a writer that skips its fsyncs wrote the batch
            synced_to(tmp.path(), kept, UNSYNCED_APPENDS);
            damage(tmp.path(), file, fault, kept);
            reader_is_sound(tmp.path(), victim, &case);
            let before = snapshot(tmp.path());
            let refused = try_put(tmp.path(), victim + 3).err();
            assert!(
                matches!(refused, Some(OdbError::Dangling { .. })),
                "{case}: {refused:?}"
            );
            assert_eq!(snapshot(tmp.path()), before, "{case}");
        }
    }
}

#[test]
fn with_no_boundaries_an_unfinished_end_nothing_names_is_cut_as_before() {
    for (file, class) in CASES {
        for fault in [Fault::Torn, Fault::Zeroed] {
            let case = format!("{} {fault:?} class {class}", FILES[file]);
            let tmp = alt_os::temp::tempdir().unwrap();
            let (victim, kept, _) = store(tmp.path(), class);
            // a writer that skips its fsyncs wrote the batch
            synced_to(tmp.path(), kept, UNSYNCED_APPENDS);
            // the records that name the content never arrived either
            for (above, len) in kept.iter().enumerate().skip(file + 1) {
                cut(tmp.path(), above, *len);
            }
            damage(tmp.path(), file, fault, kept);
            reader_is_sound(tmp.path(), victim, &case);
            recovered(tmp.path(), victim, &case);
        }
    }
}

#[test]
fn a_writer_refuses_a_file_that_ends_short_of_its_synced_records() {
    // a file and a kind of object that grows it
    for (file, class) in [(0, 0), (1, 1), (2, 2), (3, 0)] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let (victim, kept, full) = store(tmp.path(), class);
        cut(tmp.path(), file, kept[file]);
        let before = snapshot(tmp.path());
        let odb = open(tmp.path());
        for n in 0..KEPT {
            reads(&odb, n);
        }
        let lost = odb.check_synced().unwrap_err();
        drop(odb);
        let refused = try_put(tmp.path(), victim + 3).err();
        for error in [Some(lost), refused] {
            match error {
                Some(OdbError::Lost { synced, holds, .. }) => {
                    assert_eq!((synced, holds), (full[file], kept[file]), "{}", FILES[file]);
                }
                other => panic!("{}: {other:?}", FILES[file]),
            }
        }
        assert_eq!(snapshot(tmp.path()), before, "{}", FILES[file]);
    }
}

/// An older alt cuts the pack back without taking the note back, and
/// replaces the marker with its own. What it then appends inside the
/// noted length and loses to a power cut is not taken for synced.
#[test]
fn a_pack_noted_as_synced_is_checked_again_once_an_older_alt_wrote_the_marker() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let (victim, kept, _) = store(tmp.path(), 0);
    older_marker(tmp.path(), kept);
    damage(tmp.path(), 0, Fault::Zeroed, kept);
    reader_is_sound(tmp.path(), victim, "older marker");
    recovered(tmp.path(), victim, "older marker");
}

/// The note counts only next to a head record that says the store keeps
/// record boundaries: a program that writes head records and no
/// boundaries does not keep the note either.
#[test]
fn a_note_without_a_head_record_that_says_boundaries_are_kept_is_not_believed() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let (victim, kept, _) = store(tmp.path(), 0);
    rewrite(tmp.path(), |marker| {
        marker.eof = kept;
        marker.whole = kept;
    });
    overwrite_lock(tmp.path(), 0, 1176);
    damage(tmp.path(), 0, Fault::Zeroed, kept);
    recovered(tmp.path(), victim, "no head record");
}
