//! Handles held while records are taken out of the store and as many
//! appended after them: the files are as long as the handles read them,
//! and what the handles hold of them is of records that are gone. The
//! marker's generation tells of it, and the handles read the store anew.

use alt_odb::NativeOdb;

use super::dangling::{Fault, UNSYNCED_APPENDS, damage, store, synced_to};
use super::left_unsynced::left_unsynced;
use super::lost_dropped::older_alt_went_on;
use super::rewind_interrupted::{lens, object, open, put, reads};

#[test]
fn handles_held_across_a_cut_that_was_appended_over_read_the_store_anew() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    // a batch left unsynced in the running boot, as an add leaves it
    let (victim, kept, whole) = store(dir, 0);
    synced_to(dir, kept, UNSYNCED_APPENDS);
    left_unsynced(dir, kept, true);
    let mut held = open(dir);
    let unsynced = alt_os::durability::leave_unsynced(true);
    held.refresh().unwrap();
    drop(unsynced);
    let mut reader = NativeOdb::reading(dir);
    let lost = object(victim).0;
    assert!(held.contains(&lost).unwrap() && reader.contains(&lost).unwrap());
    reader.follow().unwrap();
    // its content goes in that boot; a writer that syncs cuts the
    // records and stores an object of the same length
    damage(dir, 0, Fault::Missing, kept);
    let mut other = open(dir);
    put(&mut other, victim + 3..victim + 4);
    drop(other);
    assert_eq!(lens(dir), whole, "the cut was appended over to the byte");
    held.refresh().unwrap();
    assert!(!held.contains(&lost).unwrap());
    reads(&held, victim + 3);
    reader.follow().unwrap();
    assert!(!reader.contains(&lost).unwrap());
    reads(&reader, victim + 3);
    put(&mut held, victim..victim + 1);
    reader.follow().unwrap();
    for odb in [&held, &reader] {
        reads(odb, victim);
        reads(odb, victim + 3);
    }
}

#[test]
fn handles_opened_before_a_removal_of_lost_records_read_the_store_anew() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let victim = older_alt_went_on(dir, 0, 0, &[vec![100, 101, 102]]);
    // a daemon started as the removal began: neither handle has held
    // the write lock yet
    let mut held = open(dir);
    let mut reader = NativeOdb::reading(dir);
    let lost = object(victim).0;
    assert!(held.contains(&lost).unwrap() && reader.contains(&lost).unwrap());
    let before = lens(dir);
    let mut other = open(dir);
    let found = other.lost().unwrap();
    assert!(other.drop_lost(&found).unwrap());
    // and another command brings the log back to its length
    put(&mut other, 103..104);
    drop(other);
    assert_eq!(lens(dir)[3], before[3]);
    held.refresh().unwrap();
    reader.follow().unwrap();
    for odb in [&held, &reader] {
        assert!(!odb.contains(&lost).unwrap());
        // the handle that only reads was not given the prism 101 is
        // stored through
        for n in [100, 102, 103] {
            reads(odb, n);
        }
    }
    reads(&held, 101);
    put(&mut held, victim..victim + 1);
    reads(&held, victim);
}
