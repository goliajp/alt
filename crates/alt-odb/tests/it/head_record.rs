//! The record of packs and index runs in the lock file is advice: whatever
//! it says, and however out of date it is, the store reads the same.

use std::fs;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_odb::NativeOdb;
use alt_store::{BlobOptions, Options};

fn object(n: usize) -> (ObjectId, Vec<u8>) {
    let data = format!("object {n} behind the head record\n")
        .repeat(2 + n % 3)
        .into_bytes();
    (
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &data),
        data,
    )
}

/// Small packs, so the store has several and keeps adding more.
fn opts() -> BlobOptions {
    BlobOptions {
        chunks: Options {
            seal_threshold: 6_000,
            ..Options::default()
        },
        ..BlobOptions::default()
    }
}

fn put(dir: &Path, range: std::ops::Range<usize>) {
    let mut odb = NativeOdb::open_with(dir, opts()).unwrap();
    for n in range {
        let (oid, data) = object(n);
        odb.put(oid, ObjectKind::Blob, &data).unwrap();
    }
    odb.flush().unwrap();
}

fn holds(dir: &Path, count: usize) {
    for odb in [NativeOdb::deferred(dir), NativeOdb::open(dir).unwrap()] {
        for n in 0..count {
            let (oid, data) = object(n);
            let got = odb.get(&oid).unwrap();
            assert_eq!(got.map(|o| o.data), Some(data), "object {n}");
        }
        assert!(odb.get(&object(count).0).unwrap().is_none());
        assert_eq!(odb.check().unwrap().objects, count as u64);
    }
}

fn packs(dir: &Path) -> usize {
    fs::read_dir(dir.join("store"))
        .unwrap()
        .filter(|e| {
            let name = e.as_ref().unwrap().file_name();
            name.to_string_lossy().ends_with(".altpack")
        })
        .count()
}

#[test]
fn a_writer_records_the_store_in_the_lock_file() {
    let tmp = alt_os::temp::tempdir().unwrap();
    put(tmp.path(), 0..400);
    let head = fs::read(tmp.path().join("odb.lock")).unwrap();
    assert!(
        head.starts_with(b"ALTH"),
        "the lock file holds a head record"
    );
    holds(tmp.path(), 400);
    // the record is what names the runs: with the run directory emptied a
    // reader still opens (it falls back) and finds everything
    for idx in ["map.alt.idx", "store/chunks.idx"] {
        fs::remove_dir_all(tmp.path().join(idx)).unwrap();
    }
    holds(tmp.path(), 400);
}

#[test]
fn a_record_older_than_the_store_is_seen_through() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let lock = tmp.path().join("odb.lock");
    put(tmp.path(), 0..400);
    let early = fs::read(&lock).unwrap();
    let early_packs = packs(tmp.path());

    // what a program that does not maintain the record leaves behind: more
    // packs, longer logs, merged runs, and the record as it was
    put(tmp.path(), 400..1500);
    assert!(packs(tmp.path()) > early_packs + 2);
    let late = fs::read(&lock).unwrap();
    assert_ne!(early, late);
    fs::write(&lock, &early).unwrap();
    holds(tmp.path(), 1500);

    // and after a compaction, which replaces every pack the record names
    let mut odb = NativeOdb::open_with(tmp.path(), opts()).unwrap();
    odb.compact().unwrap();
    odb.flush().unwrap();
    drop(odb);
    for stale in [&early, &late] {
        fs::write(&lock, stale).unwrap();
        holds(tmp.path(), 1500);
    }
}

#[test]
fn a_record_that_is_garbage_or_absent_is_ignored() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let lock = tmp.path().join("odb.lock");
    put(tmp.path(), 0..400);
    let good = fs::read(&lock).unwrap();
    let mut flipped = good.clone();
    flipped[18] ^= 0x10;
    for bytes in [
        &b""[..],
        &b"not a record at all"[..],
        &flipped[..],
        &good[..good.len() - 3],
    ] {
        fs::write(&lock, bytes).unwrap();
        holds(tmp.path(), 400);
    }
    // the next writer puts a good one back
    fs::write(&lock, b"").unwrap();
    put(tmp.path(), 400..410);
    assert!(fs::read(&lock).unwrap().starts_with(b"ALTH"));
    holds(tmp.path(), 410);
}

#[test]
fn a_reader_gives_a_store_without_a_record_one() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let lock = tmp.path().join("odb.lock");
    put(tmp.path(), 0..400);
    // as a program that keeps no record leaves it
    fs::write(&lock, b"").unwrap();
    let odb = NativeOdb::deferred(tmp.path());
    assert!(odb.get(&object(7).0).unwrap().is_some());
    drop(odb);
    assert!(fs::read(&lock).unwrap().starts_with(b"ALTH"));
    holds(tmp.path(), 400);
}

#[test]
fn a_handle_held_open_keeps_what_another_writer_recorded() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let lock = tmp.path().join("odb.lock");
    let mut held = NativeOdb::open_with(tmp.path(), opts()).unwrap();
    let (oid, data) = object(100_000);
    held.put(oid, ObjectKind::Blob, &data).unwrap();
    held.flush().unwrap();

    // another writer indexes a few hundred objects and records the runs
    put(tmp.path(), 0..400);
    let recorded = fs::read(&lock).unwrap();

    // the first handle writes again: one more record, no new run, so the
    // record of runs and packs must come out as the other writer left it
    let (oid, data) = object(100_001);
    held.put(oid, ObjectKind::Blob, &data).unwrap();
    held.flush().unwrap();
    assert_eq!(fs::read(&lock).unwrap(), recorded);
    drop(held);
    holds_all(tmp.path(), (0..400).chain([100_000, 100_001]));
}

fn holds_all(dir: &Path, objects: impl Iterator<Item = usize>) {
    let odb = NativeOdb::deferred(dir);
    for n in objects {
        let (oid, data) = object(n);
        assert_eq!(
            odb.get(&oid).unwrap().map(|o| o.data),
            Some(data),
            "object {n}"
        );
    }
}
