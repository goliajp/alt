//! A handle held across requests by a reader that asks for nothing new
//! ([`NativeOdb::follow_behind_writer`]): while a writer is in the middle
//! of a batch the handle reads on as it was, where a handle that follows
//! every append would open the store anew and wait for the batch to end.

use std::sync::mpsc::channel;
use std::time::Duration;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;

fn object(n: usize) -> (ObjectId, Vec<u8>) {
    let data = format!("object {n} behind a writer\n").into_bytes();
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data),
        data,
    )
}

/// The time limit is for a read that waits for the lock of the writer,
/// which this thread holds until the read is over.
fn read(reader: NativeOdb, n: usize) -> (NativeOdb, bool) {
    let (done, waited) = channel();
    std::thread::spawn(move || {
        let held = reader.contains(&object(n).0).unwrap();
        let _ = done.send((reader, held));
    });
    waited
        .recv_timeout(Duration::from_secs(60))
        .expect("the read waits for the writer")
}

#[test]
fn a_handle_behind_a_writer_reads_on_until_the_batch_is_over() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let mut writer = NativeOdb::open(tmp.path()).unwrap();
    let (oid, data) = object(0);
    writer.put(oid, ObjectKind::Blob, &data).unwrap();
    writer.flush().unwrap();

    let mut reader = NativeOdb::reading(tmp.path());
    assert!(reader.contains(&oid).unwrap());
    // nothing changed: either way of following keeps the handle
    reader.follow_behind_writer().unwrap();

    writer.begin_batch().unwrap();
    let (oid, data) = object(1);
    writer.put(oid, ObjectKind::Blob, &data).unwrap();
    reader.follow_behind_writer().unwrap();
    let (reader, held) = read(reader, 0);
    assert!(held);
    // what the batch brought is not seen
    let (mut reader, held) = read(reader, 1);
    assert!(!held);

    writer.flush().unwrap();
    reader.follow_behind_writer().unwrap();
    assert!(reader.contains(&oid).unwrap());
}
