//! Two handles on one store in one thread: the second must never wait for
//! the write batch the first has open.

use std::sync::mpsc;
use std::time::Duration;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;

/// Content that is stored as several chunks, so reading it needs the blob
/// map, which a deferred handle opens on first use.
fn large() -> (ObjectId, Vec<u8>) {
    let mut data = Vec::new();
    let mut word = 0x9e37_79b9_7f4a_7c15u64;
    while data.len() < 600_000 {
        word ^= word << 13;
        word ^= word >> 7;
        word ^= word << 17;
        data.extend_from_slice(&word.to_le_bytes());
    }
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data),
        data,
    )
}

fn small(n: u8) -> (ObjectId, Vec<u8>) {
    let data = vec![n; 40];
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data),
        data,
    )
}

/// Runs `scenario` on a thread of its own and fails when it does not
/// return: a thread waiting on a lock it holds never does.
fn finishes(scenario: impl FnOnce() + Send + 'static) {
    let (done, wait) = mpsc::channel();
    std::thread::spawn(move || {
        scenario();
        let _ = done.send(());
    });
    wait.recv_timeout(Duration::from_secs(30))
        .expect("the scenario waits on a lock its own thread holds");
}

#[test]
fn a_handle_opened_earlier_reads_while_another_has_a_batch_open() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path().to_owned();
    let (big, big_data) = large();
    {
        let mut odb = NativeOdb::open(&dir).unwrap();
        odb.put(big, ObjectKind::Blob, &big_data).unwrap();
        odb.flush().unwrap();
    }
    finishes(move || {
        let mut writer = NativeOdb::deferred(&dir);
        let reader = NativeOdb::deferred(&dir);
        let (oid, data) = small(1);
        // the reader has opened the store, but not yet its blob map
        assert!(reader.get(&oid).unwrap().is_none());
        writer.put(oid, ObjectKind::Blob, &data).unwrap();
        // the writer holds the write lock until its flush
        assert_eq!(reader.get(&big).unwrap().unwrap().data, big_data);
        writer.flush().unwrap();
    });
}

#[test]
fn a_handle_first_used_mid_batch_opens_without_waiting() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path().to_owned();
    let (big, big_data) = large();
    {
        let mut odb = NativeOdb::open(&dir).unwrap();
        odb.put(big, ObjectKind::Blob, &big_data).unwrap();
        odb.flush().unwrap();
    }
    finishes(move || {
        let mut writer = NativeOdb::deferred(&dir);
        let (oid, data) = small(2);
        writer.put(oid, ObjectKind::Blob, &data).unwrap();
        let late = NativeOdb::deferred(&dir);
        assert_eq!(late.get(&big).unwrap().unwrap().data, big_data);
        // what the batch has appended so far is on disk and readable
        assert_eq!(late.get(&oid).unwrap().unwrap().data, data);
        writer.flush().unwrap();
        // the lock is free again: another thread's writer gets it
        let (oid, data) = small(3);
        std::thread::spawn(move || {
            let mut other = NativeOdb::deferred(&dir);
            other.put(oid, ObjectKind::Blob, &data).unwrap();
            other.flush().unwrap();
        })
        .join()
        .unwrap();
    });
}
