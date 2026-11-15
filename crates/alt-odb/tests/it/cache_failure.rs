//! The index runs are a cache: where they cannot be written (a read-only
//! file system, a full disk) lookups get slower, and nothing else changes.

use std::os::unix::fs::symlink;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;

fn object(n: usize) -> (ObjectId, Vec<u8>) {
    let data = format!("object {n} with no index\n").into_bytes();
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data),
        data,
    )
}

/// Replaces the index directories with dangling links: they read as
/// absent, and nothing can be created under them, whoever tries.
fn block_indexes(dir: &Path) {
    for idx in ["map.alt.idx", "store/chunks.idx"] {
        let path = dir.join(idx);
        let _ = std::fs::remove_dir_all(&path);
        symlink(dir.join("nowhere/at/all"), &path).unwrap();
    }
}

fn put(odb: &mut NativeOdb, range: std::ops::Range<usize>) {
    for n in range {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
}

fn holds(dir: &Path, count: usize) {
    let odb = NativeOdb::deferred(dir);
    for n in 0..count {
        let (oid, data) = object(n);
        assert_eq!(odb.get(&oid).unwrap().unwrap().data, data, "object {n}");
    }
}

#[test]
fn an_open_that_cannot_index_a_long_tail_still_opens() {
    let tmp = alt_os::temp::tempdir().unwrap();
    {
        let mut odb = NativeOdb::open(tmp.path()).unwrap();
        put(&mut odb, 0..700);
        odb.flush().unwrap();
    }
    block_indexes(tmp.path());
    // 700 records past the (absent) runs: every open tries to index them
    holds(tmp.path(), 700);
    let odb = NativeOdb::open(tmp.path()).unwrap();
    assert_eq!(odb.len().unwrap(), 700);
}

#[test]
fn a_flush_that_cannot_index_still_makes_the_batch_durable() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let mut odb = NativeOdb::open(tmp.path()).unwrap();
    put(&mut odb, 0..10);
    odb.flush().unwrap();
    block_indexes(tmp.path());
    put(&mut odb, 10..700);
    odb.flush().unwrap();
    // the batch reached its fsync: the durable marker covers map.alt
    let durable = std::fs::read(tmp.path().join("odb.durable")).unwrap();
    let map_len = std::fs::metadata(tmp.path().join("map.alt")).unwrap().len();
    let marked = u64::from_le_bytes(durable[16..24].try_into().unwrap());
    assert_eq!(
        marked, map_len,
        "the flush fsynced up to the end of map.alt"
    );
    // and the lock is free again
    put(&mut odb, 700..710);
    odb.flush().unwrap();
    drop(odb);
    holds(tmp.path(), 710);
}
