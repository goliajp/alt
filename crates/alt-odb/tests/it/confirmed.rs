//! A handle that does not hold the write lock passes over the records
//! that were never synced and name content which is not stored: it says
//! of no object that the store holds it unless the object reads. Which
//! records were synced it takes from the note in the lock file, and from
//! the marker only where a file is longer than the note says.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_git_codec::ObjectKind;
use alt_odb::{BlobId, NativeOdb};

use super::dangling::{CASES, Fault, damage, snapshot, store, synced_to};
use super::marker::rewrite;
use super::rewind_interrupted::{
    FILES, KEPT, Split, cut, lens, object, older_marker, open, put, reads,
};
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

/// Index runs are written once their records are fsynced and before the
/// marker says so: a run can reach past the boundary. A reader holds the
/// log as ending where the writer will cut it all the same, behind the
/// run, and after the cut the two say the same of every object.
#[test]
fn a_reader_passes_over_unsynced_records_an_index_run_covers() {
    // one of several chunks, then enough of the other two kinds for runs
    let batch: Vec<u32> = (1000..1040)
        .chain([1042])
        .chain(1043..1900)
        .filter(|n| n % 3 != 1 || *n == 1042)
        .collect();
    let first_mapped = batch.iter().position(|n| *n == 1042).unwrap();
    for lost in [0, 1] {
        for zeroed in [false, true] {
            let case = format!("{} lost, zeroed {zeroed}", FILES[lost]);
            let tmp = alt_os::temp::tempdir().unwrap();
            let dir = tmp.path();
            let mut odb = open(dir);
            put(&mut odb, 0..KEPT);
            let kept = lens(dir);
            for n in &batch {
                let (oid, data) = object(*n);
                odb.put(oid, ObjectKind::Blob, &data).unwrap();
            }
            odb.flush().unwrap();
            drop(odb);
            let run_end = |name: std::ffi::OsString| {
                let name = name.into_string().unwrap();
                let end = name.strip_suffix(".run")?.split_once('-')?.1.to_owned();
                u64::from_str_radix(&end, 16).ok()
            };
            let runs = fs::read_dir(dir.join("map.alt.idx")).unwrap();
            let covered = runs.filter_map(|run| run_end(run.unwrap().file_name()));
            assert!(covered.max().unwrap() > kept[3], "{case}: no run to pass");
            synced_to(dir, kept, 0);
            if zeroed {
                let len = lens(dir)[lost];
                cut(dir, lost, kept[lost]);
                cut(dir, lost, len);
            } else {
                cut(dir, lost, kept[lost]);
            }
            // with the blob map lost, the objects before the first it
            // names are whole
            let held = if lost == 0 { 0 } else { first_mapped };
            let holds = |odb: &NativeOdb| -> Vec<bool> {
                let asked = (0..KEPT).chain(batch.iter().copied());
                asked.map(|n| odb.contains(&object(n).0).unwrap()).collect()
            };
            let before = snapshot(dir);
            let found = [holds(&reading(dir)), holds(&open(dir))];
            assert_eq!(snapshot(dir), before, "{case}");
            for found in &found {
                let expect = KEPT as usize + held;
                assert!(found[..expect].iter().all(|held| *held), "{case}");
                assert!(found[expect..].iter().all(|held| !*held), "{case}");
            }
            let reader = reading(dir);
            for n in (0..KEPT).chain(batch[..held].iter().copied()) {
                reads(&reader, n);
            }
            let (oid, _) = object(batch[held]);
            assert!(reader.get(&oid).unwrap().is_none(), "{case}");
            drop(reader);
            // the write lock taken and given up: the records are cut
            open(dir).checkpoint().unwrap();
            assert!(lens(dir)[3] < before[3].0, "{case}: nothing was cut");
            assert_eq!(holds(&reading(dir)), found[0], "{case}");
        }
    }
}
