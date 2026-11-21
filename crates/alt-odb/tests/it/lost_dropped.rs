//! Records naming content the store does not hold, with records after
//! them that were synced: a writer reports them and cuts nothing; taken
//! out on request, they leave every other record where a lookup finds it.

use std::path::Path;

use alt_git_codec::ObjectKind;
use alt_odb::OdbError;

use super::dangling::{CASES, Fault, damage, snapshot, store, synced_to, try_put};
use super::left_unsynced::left_unsynced;
use super::rewind_interrupted::{ALL, FILES, KEPT, lens, object, open, put, reads};

/// What an older alt, first to write after a power cut, leaves of a
/// store whose last batch lost its content in `file`: the files recorded
/// as synced the way it found them, each batch of `after` stored and
/// synced past them, and the note of the boot before still there.
fn older_alt_went_on(dir: &Path, file: usize, class: u32, after: &[Vec<u32>]) -> u32 {
    let (victim, kept, _) = store(dir, class);
    damage(dir, file, Fault::Missing, kept);
    synced_to(dir, lens(dir), 0);
    let mut odb = open(dir);
    for batch in after {
        for n in batch {
            let (oid, data) = object(*n);
            odb.put(oid, ObjectKind::Blob, &data).unwrap();
        }
        odb.flush().unwrap();
    }
    drop(odb);
    left_unsynced(dir, kept, false);
    victim
}

/// Objects stored after the lost one: some of each kind; or, of the
/// smallest kind, a batch long enough to get an index run with the
/// records before it, and a few more that no run covers.
fn stored_after(victim: u32, indexed: bool) -> Vec<Vec<u32>> {
    match indexed {
        true => vec![
            (0..400).map(|n| 300 + 3 * n).collect(),
            (0..5).map(|n| 3000 + 3 * n).collect(),
        ],
        false => vec![(victim + 10..victim + 16).collect()],
    }
}

#[test]
fn records_naming_lost_content_are_taken_out_and_the_synced_ones_after_them_kept() {
    for (file, class) in CASES {
        for indexed in [false, true] {
            let case = format!("{} class {class} indexed {indexed}", FILES[file]);
            let tmp = alt_os::temp::tempdir().unwrap();
            let dir = tmp.path();
            let after = stored_after(ALL + class, indexed);
            let victim = older_alt_went_on(dir, file, class, &after);
            let after = after.concat();
            let refused = try_put(dir, victim + 3).err();
            assert!(
                matches!(refused, Some(OdbError::Dangling { .. })),
                "{case}: {refused:?}"
            );
            let before = snapshot(dir);
            let mut odb = open(dir);
            let lost = odb.lost().unwrap();
            assert_eq!(
                lost.objects,
                [(object(victim).0, ObjectKind::Blob)],
                "{case}"
            );
            assert!(
                lost.records.iter().any(|(log, _)| *log == "map.alt"),
                "{case}"
            );
            assert_eq!(snapshot(dir), before, "{case}: looking changes nothing");
            assert!(odb.drop_lost(&lost).unwrap(), "{case}");
            // the handle that took them out goes on
            assert!(!odb.contains(&object(victim).0).unwrap(), "{case}");
            reads(&odb, after[0]);
            drop(odb);
            for (shorter, was) in lens(dir).iter().zip(before).skip(1) {
                assert!(*shorter <= was.0, "{case}");
            }
            let mut odb = try_put(dir, victim + 3).unwrap_or_else(|e| panic!("{case}: {e:?}"));
            assert!(odb.lost().unwrap().is_empty(), "{case}");
            assert!(!odb.contains(&object(victim).0).unwrap(), "{case}");
            put(&mut odb, victim..victim + 1);
            drop(odb);
            let odb = open(dir);
            for n in (0..KEPT)
                .chain(after.iter().copied())
                .chain([victim, victim + 3])
            {
                reads(&odb, n);
            }
            odb.check().unwrap_or_else(|e| panic!("{case}: {e:?}"));
        }
    }
}

#[test]
fn a_store_that_moved_since_its_lost_records_were_listed_is_left_as_it_is() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let victim = older_alt_went_on(dir, 0, 0, &[vec![100, 101, 102]]);
    let mut odb = open(dir);
    let listed = odb.lost().unwrap();
    // the records are taken out through another handle, and one more
    // object is stored
    let mut other = open(dir);
    let found = other.lost().unwrap();
    assert!(other.drop_lost(&found).unwrap());
    put(&mut other, 103..104);
    drop(other);
    let before = snapshot(dir);
    assert!(!odb.drop_lost(&listed).unwrap());
    assert_eq!(snapshot(dir), before);
    drop(odb);
    let odb = open(dir);
    for n in (0..KEPT).chain(100..104) {
        reads(&odb, n);
    }
    assert!(!odb.contains(&object(victim).0).unwrap());
    odb.check().unwrap();
}

#[test]
fn a_store_with_no_lost_record_has_none_listed_and_nothing_to_take_out() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let mut odb = open(dir);
    put(&mut odb, 0..KEPT);
    let before = snapshot(dir);
    let lost = odb.lost().unwrap();
    assert!(lost.is_empty() && lost.objects.is_empty());
    assert!(odb.drop_lost(&lost).unwrap());
    assert_eq!(snapshot(dir), before);
}
