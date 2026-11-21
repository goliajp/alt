//! Rolling back one batch of a store that another handle also writes to:
//! only that batch goes. What the other handle appended before the batch
//! began stays, and the other handle appends nothing while the batch is
//! open, nor takes an object of the batch for stored.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use alt_git_codec::ObjectKind;
use alt_odb::{NativeOdb, OdbCheckpoint};

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
/// back.
#[test]
fn a_handle_that_puts_the_objects_of_an_open_batch_stores_them_after_its_rollback() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let mut odb = open(tmp.path());
    put(&mut odb, 0..9);
    let begun = batch(&mut odb, 9..18);
    let dir = tmp.path().to_owned();
    let (started, starts) = mpsc::channel();
    let other = thread::spawn(move || {
        started.send(()).unwrap();
        put(&mut open(&dir), 9..18);
    });
    starts.recv().unwrap();
    // long enough for the other handle to get to the lock
    thread::sleep(Duration::from_millis(300));
    odb.rewind(begun).unwrap();
    odb.flush_pending().unwrap();
    other.join().unwrap();
    let read = open(tmp.path());
    for n in 0..18 {
        reads(&read, n);
    }
    read.check().unwrap();
}
