//! A handle that does not hold the write lock passes over the records
//! that were never synced and name content which is not stored: it says
//! of no object that the store holds it unless the object reads. Which
//! records were synced it takes from the note in the lock file, and from
//! the marker only where a file is longer than the note says.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_odb::{BlobId, NativeOdb};

use super::dangling::{CASES, Fault, damage, snapshot, store, synced_to};
use super::marker::rewrite;
use super::rewind_interrupted::{FILES, KEPT, Split, cut, object, older_marker, open, reads};
use super::synced_note::{NOTE_AT, note_of, write_note};

fn reading(dir: &Path) -> NativeOdb {
    let mut odb = NativeOdb::reading(dir);
    odb.register_prism(Box::new(Split)).unwrap();
    odb
}

/// The handle holds the synced objects and knows nothing of `victim`.
fn passes_over(odb: &NativeOdb, victim: u32, case: &str) {
    for n in 0..KEPT {
        reads(odb, n);
    }
    let (oid, data) = object(victim);
    assert!(!odb.contains(&oid).unwrap(), "{case}");
    assert!(odb.lookup(&oid).unwrap().is_none(), "{case}");
    assert!(odb.get(&oid).unwrap().is_none(), "{case}");
    assert!(
        odb.get_content(BlobId::of(&data)).unwrap().is_none(),
        "{case}"
    );
}

#[test]
fn a_reader_holds_no_record_that_was_never_synced_and_names_lost_content() {
    for (file, class) in CASES {
        for fault in [Fault::Missing, Fault::Torn, Fault::Zeroed] {
            let case = format!("{} {fault:?} class {class}", FILES[file]);
            let tmp = alt_os::temp::tempdir().unwrap();
            let (victim, kept, _) = store(tmp.path(), class);
            synced_to(tmp.path(), kept, 0);
            damage(tmp.path(), file, fault, kept);
            let before = snapshot(tmp.path());
            passes_over(&reading(tmp.path()), victim, &case);
            passes_over(&open(tmp.path()), victim, &case);
            assert_eq!(snapshot(tmp.path()), before, "{case}");
            // a check is shown the records as they are
            let (oid, _) = object(victim);
            let check = NativeOdb::read_only(tmp.path());
            assert!(check.contains(&oid).unwrap(), "{case}");
        }
    }
}

#[test]
fn a_reader_passes_over_what_follows_the_lengths_an_older_marker_records() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let (victim, kept, _) = store(tmp.path(), 0);
    older_marker(tmp.path(), kept);
    damage(tmp.path(), 0, Fault::Missing, kept);
    for with_note in [true, false] {
        if with_note {
            // as this alt left it before the older one appended
            write_note(tmp.path(), kept);
        } else {
            let lock = fs::OpenOptions::new()
                .write(true)
                .open(tmp.path().join("odb.lock"))
                .unwrap();
            lock.write_all_at(&[0u8; 48], NOTE_AT as u64).unwrap();
        }
        let before = snapshot(tmp.path());
        passes_over(&reading(tmp.path()), victim, "older marker");
        assert_eq!(snapshot(tmp.path()), before);
    }
}

/// The note stands in for the marker: with every file no longer than the
/// note says, the marker is not read, whatever it holds.
#[test]
fn a_store_no_longer_than_its_note_says_is_not_looked_up_in_the_marker() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let (victim, kept, full) = store(tmp.path(), 0);
    assert_eq!(note_of(tmp.path()), Some((1, full)));
    // a state no writer leaves: the note past the marker
    rewrite(tmp.path(), |marker| {
        marker.eof = kept;
        marker.whole = kept;
    });
    damage(tmp.path(), 0, Fault::Missing, kept);
    let (oid, _) = object(victim);
    assert!(reading(tmp.path()).contains(&oid).unwrap());
    // a note that does not read is no note
    let mut lock = fs::read(tmp.path().join("odb.lock")).unwrap();
    lock[NOTE_AT + 20] ^= 1;
    fs::write(tmp.path().join("odb.lock"), lock).unwrap();
    passes_over(&reading(tmp.path()), victim, "damaged note");
}

/// The blob map and the Tier 1 map are opened by the first read that
/// needs them: each is then held by the same rule, also when `map.alt`
/// itself is no longer than the note says.
#[test]
fn a_map_opened_late_is_held_without_its_unsynced_records_too() {
    for class in [1, 2] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let (victim, kept, full) = store(tmp.path(), class);
        // the record of `map.alt` never arrived, nor did the content
        cut(tmp.path(), 3, kept[3]);
        cut(tmp.path(), 0, kept[0]);
        rewrite(tmp.path(), |marker| {
            marker.eof = kept;
            marker.whole = kept;
        });
        write_note(tmp.path(), kept);
        assert!(full[class as usize] > kept[class as usize]);
        let before = snapshot(tmp.path());
        let odb = reading(tmp.path());
        let (_, data) = object(victim);
        let read = odb.get_content(BlobId::of(&data));
        assert!(matches!(read, Ok(None)), "class {class}: {read:?}");
        for n in 0..KEPT {
            reads(&odb, n);
        }
        assert_eq!(snapshot(tmp.path()), before, "class {class}");
    }
}

/// What grows back where whole records were cut was not synced: the
/// writer that cuts them takes the note back first.
#[test]
fn a_writer_that_cuts_whole_records_takes_the_note_back() {
    for file in [0, 1, 2] {
        let tmp = alt_os::temp::tempdir().unwrap();
        let (_, kept, full) = store(tmp.path(), file as u32);
        rewrite(tmp.path(), |marker| {
            marker.eof = kept;
            marker.whole = kept;
        });
        // the content is gone at a record boundary: nothing is unfinished
        cut(tmp.path(), 0, kept[0]);
        assert_eq!(note_of(tmp.path()), Some((1, full)));
        let mut odb = open(tmp.path());
        // takes the write lock and gives it up without writing
        odb.checkpoint().unwrap();
        assert_eq!(note_of(tmp.path()), None, "{}", FILES[file]);
        assert!(fs::metadata(tmp.path().join(FILES[3])).unwrap().len() < full[3]);
    }
}

#[test]
fn the_note_records_all_four_files_as_far_as_the_marker_does() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let (_, _, full) = store(tmp.path(), 2);
    assert_eq!(note_of(tmp.path()), Some((1, full)));
    assert_eq!(super::marker::recorded(tmp.path()).whole, full);
}
