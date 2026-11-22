//! Rolling back one batch of a store that another handle also writes to:
//! only that batch goes. What the other handle appended before the batch
//! began stays, and the other handle appends nothing while the batch is
//! open, nor takes an object of the batch for stored.

use std::fs::File;
use std::io::ErrorKind;
use std::thread;

use alt_git_codec::ObjectKind;
use alt_odb::{NativeOdb, OdbCheckpoint};
use alt_os::sys::{LOCK_NB, LOCK_SH, flock};

use super::rewind_interrupted::{object, open, put, reads};

/// A batch of `objects`, left open: where it began.
fn batch(odb: &mut NativeOdb, objects: std::ops::Range<u32>) -> OdbCheckpoint {
    let begun = odb.begin_batch().unwrap();
    for n in objects {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
    begun
}

#[test]
fn a_batch_rolled_back_leaves_what_another_handle_wrote_before_it_began() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let mut odb = open(tmp.path());
    put(&mut odb, 0..9);
    // appended after `odb` last read the files
    put(&mut open(tmp.path()), 9..18);
    let begun = batch(&mut odb, 18..27);
    odb.rewind(begun).unwrap();
    odb.flush_pending().unwrap();
    let read = open(tmp.path());
    for n in 0..18 {
        reads(&read, n);
    }
    for n in 18..27 {
        assert!(!read.contains(&object(n).0).unwrap(), "object {n}");
    }
    read.check().unwrap();
}

/// Another handle puts the objects of an open batch. It would find them
/// stored and write nothing, were it let in before the batch is rolled
/// back: the lock it has to wait for, to open the store and to put, is
/// held for as long as the batch is open.
#[test]
fn a_handle_that_puts_the_objects_of_an_open_batch_stores_them_after_its_rollback() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let mut odb = open(tmp.path());
    put(&mut odb, 0..9);
    let begun = batch(&mut odb, 9..18);
    let lock = File::open(tmp.path().join("odb.lock")).unwrap();
    let shared = flock(&lock, LOCK_SH | LOCK_NB);
    assert_eq!(shared.unwrap_err().kind(), ErrorKind::WouldBlock);
    let dir = tmp.path().to_owned();
    let other = thread::spawn(move || put(&mut open(&dir), 9..18));
    odb.rewind(begun).unwrap();
    odb.flush_pending().unwrap();
    other.join().unwrap();
    let read = open(tmp.path());
    for n in 0..18 {
        reads(&read, n);
    }
    read.check().unwrap();
}
