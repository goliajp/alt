//! Whole records cut off the active pack and no record of a log that
//! names content with them, the pack then grown back to the length it
//! had: a handle that read the records which went holds them in its
//! index, and the length of the pack tells it nothing. The cut moves the
//! store to another generation, by which the handle reads it anew.

use std::fs;
use std::os::unix::fs::FileExt;

use super::boot_runs::put_unsynced;
use super::rewind_interrupted::{FILES, cut, lens, open, put, reads};

#[test]
fn a_handle_that_held_records_cut_off_the_pack_alone_stores_their_content_again() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let mut held = open(dir);
    put(&mut held, 0..30);
    let synced = lens(dir);
    // the chunks of an object, left unsynced by a writer that went
    // before a record named them
    put_unsynced(dir, 7002..7003);
    let full = lens(dir);
    assert!(full[0] > synced[0]);
    for (file, len) in synced.iter().enumerate().skip(1) {
        cut(dir, file, *len);
    }
    {
        // the handle reads what the pack holds now and syncs nothing
        let _unsynced = alt_os::durability::leave_unsynced(true);
        held.refresh().unwrap();
    }
    assert!(
        !held
            .contains(&super::rewind_interrupted::object(7002).0)
            .unwrap()
    );
    let generation = held.generation();

    // the first of those records no longer reads: a head of a kind
    // there is none of
    let pack = fs::OpenOptions::new().write(true).open(dir.join(FILES[0]));
    let pack = pack.unwrap();
    pack.write_all_at(&[0xff], synced[0] + 32).unwrap();
    pack.write_all_at(&[0; 4], synced[0] + 37).unwrap();
    // a writer cuts the pack there and stores an object as long as the
    // one whose chunks went
    let mut other = open(dir);
    put(&mut other, 7005..7006);
    let cut_there = other.take_discarded();
    let cut_there = cut_there.iter().find(|cut| cut.file.ends_with(FILES[0]));
    assert_eq!(cut_there.map(|cut| cut.from), Some(synced[0]));
    drop(other);
    assert_eq!(lens(dir)[0], full[0], "the pack is as long as it was");
    assert_ne!(held.generation(), generation, "whole records went");

    put(&mut held, 7002..7003);
    drop(held);
    let odb = open(dir);
    for n in (0..30).chain([7002, 7005]) {
        reads(&odb, n);
    }
    odb.check().unwrap();
}
