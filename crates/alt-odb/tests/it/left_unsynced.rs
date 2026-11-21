//! Appends that were left unsynced, by the boot they were made in: in the
//! running boot they are as they were written, and a record that names
//! content which is not stored is damage; after another boot it is what a
//! power cut leaves, and is cut with what it names.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_odb::OdbError;

use super::dangling::{
    CASES, Fault, UNSYNCED_APPENDS, damage, reader_is_sound, recovered, snapshot, store, synced_to,
    try_put,
};
use super::rewind_interrupted::{FILES, lens};

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

#[test]
fn in_the_boot_appends_were_left_unsynced_in_a_record_naming_nothing_is_refused() {
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
