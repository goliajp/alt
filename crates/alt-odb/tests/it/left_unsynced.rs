//! Appends that were left unsynced, by the boot they were made in: in the
//! running boot they are as they were written, and nobody looks at them;
//! after another boot they are what a power cut leaves. A record among
//! them that does not read, or names content which does not, was never
//! synced in either: every writer cuts it with what it names.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_odb::OdbError;

use super::dangling::{
    CASES, Fault, UNSYNCED_APPENDS, damage, reader_is_sound, recovered, snapshot, store, synced_to,
    try_put,
};
use super::rewind_interrupted::{FILES, KEPT, lens, object, open, put, reads};

/// Notes in the lock file that the appends past `whole` were left
/// unsynced in the running boot, or in another one.
pub(super) fn left_unsynced(dir: &Path, whole: [u64; 4], running: bool) {
    let boot = alt_os::boot::id().expect("this kernel tells its boot");
    let mut note = boot.to_vec();
    note[0] ^= u8::from(!running);
    note.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0]);
    for len in whole {
        note.extend_from_slice(&len.to_le_bytes());
    }
    let sum = alt_hash::blake3::hash(&note);
    note.extend_from_slice(&sum.as_bytes()[..8]);
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(dir.join("odb.lock"));
    lock.unwrap().write_all_at(&note, 1224).unwrap();
}

#[test]
fn appends_left_unsynced_in_another_boot_are_cut_where_they_name_content_that_never_arrived() {
    for (file, class) in CASES {
        for fault in [Fault::Missing, Fault::Torn, Fault::Zeroed] {
            // a note of another boot, and one that was lost
            for noted in [true, false] {
                let case = format!("{} {fault:?} class {class} noted {noted}", FILES[file]);
                let tmp = alt_os::temp::tempdir().unwrap();
                let (victim, kept, _) = store(tmp.path(), class);
                // a writer that skips its fsyncs wrote the batch
                synced_to(tmp.path(), kept, UNSYNCED_APPENDS);
                if noted {
                    left_unsynced(tmp.path(), kept, false);
                }
                damage(tmp.path(), file, fault, kept);
                reader_is_sound(tmp.path(), victim, &case);
                recovered(tmp.path(), victim, &case);
            }
        }
    }
}

/// Where a file ends in a record that does not read, what the running
/// boot left unsynced is not as it was written: a writer that syncs
/// nothing looks at it as at what another boot left.
#[test]
fn in_the_boot_appends_were_left_unsynced_in_a_writer_that_syncs_nothing_cuts_them_at_a_record_that_does_not_read()
 {
    for (file, class) in CASES {
        for fault in [Fault::Torn, Fault::Zeroed] {
            let case = format!("{} {fault:?} class {class}", FILES[file]);
            let tmp = alt_os::temp::tempdir().unwrap();
            let (victim, kept, _) = store(tmp.path(), class);
            // a writer that skips its fsyncs wrote the batch
            synced_to(tmp.path(), kept, UNSYNCED_APPENDS);
            left_unsynced(tmp.path(), kept, true);
            damage(tmp.path(), file, fault, kept);
            reader_is_sound(tmp.path(), victim, &case);
            let _unsynced = alt_os::durability::leave_unsynced(true);
            recovered(tmp.path(), victim, &case);
        }
    }
}

/// The cache of the kernel holds what was left unsynced only while the
/// file system under it stays: a writer that syncs is about to record
/// those appends as synced, and looks at them first.
#[test]
fn a_writer_that_syncs_cuts_what_the_running_boot_left_unsynced_and_names_content_that_is_gone() {
    for (file, class) in CASES {
        for fault in [Fault::Missing, Fault::Torn, Fault::Zeroed] {
            let case = format!("{} {fault:?} class {class}", FILES[file]);
            let tmp = alt_os::temp::tempdir().unwrap();
            let (victim, kept, _) = store(tmp.path(), class);
            synced_to(tmp.path(), kept, UNSYNCED_APPENDS);
            left_unsynced(tmp.path(), kept, true);
            damage(tmp.path(), file, fault, kept);
            reader_is_sound(tmp.path(), victim, &case);
            recovered(tmp.path(), victim, &case);
        }
    }
}

/// A record of the pack that is not its last and does not read, past
/// the boundary: to a writer that syncs, what the running boot left
/// unsynced ends there, as what another boot left does.
#[test]
fn a_writer_that_syncs_cuts_the_pack_the_running_boot_left_unsynced_at_a_record_within_it() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let mut odb = open(dir);
    put(&mut odb, 0..KEPT);
    let kept = lens(dir);
    // two objects of one chunk each, left unsynced
    put(&mut odb, 60..61);
    let first = lens(dir);
    put(&mut odb, 63..64);
    drop(odb);
    synced_to(dir, kept, UNSYNCED_APPENDS);
    left_unsynced(dir, kept, true);
    // the first of the two records is whole in length, and holds zeros
    // where the end of its content was
    let pack = fs::OpenOptions::new().write(true).open(dir.join(FILES[0]));
    pack.unwrap().write_all_at(&[0; 20], first[0] - 20).unwrap();
    let mut odb = try_put(dir, 66).unwrap();
    for n in [60, 63] {
        assert!(!odb.contains(&object(n).0).unwrap(), "object {n}");
    }
    put(&mut odb, 60..61);
    put(&mut odb, 63..64);
    drop(odb);
    let odb = open(dir);
    for n in (0..KEPT).chain([60, 63, 66]) {
        reads(&odb, n);
    }
    odb.check().unwrap();
}

fn note(dir: &Path) -> [u8; 64] {
    let mut note = [0u8; 64];
    let lock = fs::File::open(dir.join("odb.lock")).unwrap();
    lock.read_exact_at(&mut note, 1224).unwrap();
    note
}

#[test]
fn what_an_older_alt_recorded_as_synced_over_appends_of_another_boot_is_looked_at_and_not_cut() {
    for (file, class) in CASES {
        for lost in [false, true] {
            let case = format!("{} class {class} lost {lost}", FILES[file]);
            let tmp = alt_os::temp::tempdir().unwrap();
            let (victim, kept, _) = store(tmp.path(), class);
            if lost {
                damage(tmp.path(), file, Fault::Missing, kept);
            }
            // an older alt synced the files as it found them after the
            // power cut, and left the note of the boot before as it was
            synced_to(tmp.path(), lens(tmp.path()), 0);
            left_unsynced(tmp.path(), kept, false);
            let before = snapshot(tmp.path());
            let put = try_put(tmp.path(), victim + 3);
            if lost {
                let refused = put.err();
                assert!(
                    matches!(refused, Some(OdbError::Dangling { .. })),
                    "{case}: {refused:?}"
                );
                assert_eq!(snapshot(tmp.path()), before, "{case}");
                assert_ne!(note(tmp.path()), [0; 64], "{case}");
            } else {
                drop(put.unwrap_or_else(|e| panic!("{case}: {e:?}")));
                assert_eq!(note(tmp.path()), [0; 64], "{case}");
                reader_is_sound(tmp.path(), victim, &case);
            }
        }
    }
}
