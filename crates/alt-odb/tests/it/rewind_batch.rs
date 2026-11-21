//! Rolling back one batch of a store that another handle also writes to:
//! only that batch goes, whatever the other handle appended before it
//! began or after it ended.

use alt_git_codec::ObjectKind;
use alt_odb::{NativeOdb, OdbCheckpoint};

use super::rewind_interrupted::{object, open, put, reads};

/// A batch of `objects`, flushed: where it began and where it ended.
fn batch(odb: &mut NativeOdb, objects: std::ops::Range<u32>) -> (OdbCheckpoint, OdbCheckpoint) {
    let begun = odb.begin_batch().unwrap();
    for n in objects {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
    odb.flush_pending().unwrap();
    (begun, odb.cursor().unwrap())
}

#[test]
fn a_batch_rolled_back_leaves_what_another_handle_wrote_before_it_began() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let mut odb = open(tmp.path());
    put(&mut odb, 0..9);
    // appended after `odb` last read the files
    put(&mut open(tmp.path()), 9..18);
    let (begun, ended) = batch(&mut odb, 18..27);
    assert!(odb.rewind_batch(begun, ended).unwrap());
    let read = open(tmp.path());
    for n in 0..18 {
        reads(&read, n);
    }
    for n in 18..27 {
        assert!(!read.contains(&object(n).0).unwrap(), "object {n}");
    }
    read.check().unwrap();
}

#[test]
fn a_batch_is_not_rolled_back_once_another_handle_has_written_after_it() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let mut odb = open(tmp.path());
    put(&mut odb, 0..9);
    let (begun, ended) = batch(&mut odb, 9..18);
    put(&mut open(tmp.path()), 18..27);
    assert!(!odb.rewind_batch(begun, ended).unwrap());
    let read = open(tmp.path());
    for n in 0..27 {
        reads(&read, n);
    }
    read.check().unwrap();
}
