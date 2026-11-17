//! The index runs are a cache: where they cannot be written (a read-only
//! file system, a full disk) lookups get slower, and nothing else changes.
//! Damage to the files they index, found while writing them, is reported.

use std::os::unix::fs::symlink;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::{NativeOdb, OdbError};

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
fn a_store_with_a_long_tail_and_nowhere_to_index_it_still_opens() {
    let tmp = alt_os::temp::tempdir().unwrap();
    {
        let mut odb = NativeOdb::open(tmp.path()).unwrap();
        put(&mut odb, 0..700);
        odb.flush().unwrap();
    }
    block_indexes(tmp.path());
    // 700 records past the (absent) runs
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
    let marked = super::marker::recorded(tmp.path());
    let map_len = std::fs::metadata(tmp.path().join("map.alt")).unwrap().len();
    assert_eq!(
        (marked.eof[3], marked.whole[3]),
        (map_len, map_len),
        "the flush fsynced up to the end of map.alt"
    );
    // and the lock is free again
    put(&mut odb, 700..710);
    odb.flush().unwrap();
    drop(odb);
    holds(tmp.path(), 710);
}

#[test]
fn a_flush_reports_damage_it_finds_while_indexing() {
    use std::os::unix::fs::FileExt;
    let tmp = alt_os::temp::tempdir().unwrap();
    let map = tmp.path().join("map.alt");
    let mut odb = NativeOdb::open(tmp.path()).unwrap();
    put(&mut odb, 0..600);
    odb.flush().unwrap();

    // record 100 of map.alt rots behind the runs, and the runs are lost
    let file = std::fs::OpenOptions::new().write(true).open(&map).unwrap();
    file.write_all_at(&[0xff], 5 + 100 * 82 + 70).unwrap();
    std::fs::remove_dir_all(tmp.path().join("map.alt.idx")).unwrap();

    put(&mut odb, 600..900);
    let before = std::fs::metadata(&map).unwrap().len();
    let flushed = odb.flush();
    assert!(
        matches!(flushed, Err(OdbError::Format("map.alt record corrupt"))),
        "{flushed:?}"
    );
    assert_eq!(std::fs::metadata(&map).unwrap().len(), before);
}

#[test]
fn an_open_indexes_what_was_synced_and_a_write_reports_a_damaged_file() {
    let tmp = alt_os::temp::tempdir().unwrap();
    {
        let mut odb = NativeOdb::open(tmp.path()).unwrap();
        put(&mut odb, 0..700);
        odb.flush().unwrap();
    }
    // a long tail to index, and a tier1 file that is not one
    std::fs::remove_dir_all(tmp.path().join("map.alt.idx")).unwrap();
    std::fs::write(tmp.path().join("tier1"), b"not a tier1 file").unwrap();
    // a read that does not need the file goes through, and the open
    // before it indexes map.alt, every record of which was synced
    assert_eq!(NativeOdb::deferred(tmp.path()).len().unwrap(), 700);
    assert!(tmp.path().join("map.alt.idx").is_dir());
    let mut odb = NativeOdb::deferred(tmp.path());
    let (oid, data) = object(700);
    let written = odb.put(oid, ObjectKind::Blob, &data);
    assert!(
        matches!(
            written,
            Err(OdbError::Store(alt_store::StoreError::Format(_)))
        ),
        "{written:?}"
    );
}
