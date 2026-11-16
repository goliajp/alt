//! A damaged record in the middle of a log is reported and nothing is cut:
//! only a crashed writer's unfinished last record is ever cut off.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_store::reclog::RecLog;
use alt_store::{ChunkStore, Options, StoreError};

use super::index_runs::{LAYOUT, REC_LEN, chunk, record, value};

/// Flips a byte in the value of record `n`.
fn damage(path: &Path, n: u64) {
    let file = OpenOptions::new().write(true).open(path).unwrap();
    file.write_all_at(&[0xff], 5 + n * REC_LEN as u64 + 33)
        .unwrap();
}

fn len(path: &Path) -> u64 {
    fs::metadata(path).unwrap().len()
}

fn corrupt(result: Result<(), StoreError>) -> bool {
    matches!(result, Err(StoreError::Format(what)) if what == LAYOUT.corrupt)
}

#[test]
fn a_rewind_over_a_damaged_record_cuts_only_what_it_was_asked_to() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 0..600 {
        log.append(&record(n, n)).unwrap();
    }
    log.index_tail(true).unwrap();
    drop(log);
    damage(&path, 100);

    // the record is behind the runs: the log opens, and a batch is rolled
    // back after its flush merged it into the run over the damage
    let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
    let before = log.appended_len();
    for n in 1000..1300 {
        log.append(&record(n, n)).unwrap();
    }
    log.index_tail(true).unwrap();
    assert!(corrupt(log.rewind(before)), "the damage is reported");
    assert_eq!(len(&path), before, "the 600 committed records stay");
}

#[test]
fn indexing_over_a_damaged_record_leaves_the_log_and_the_handle_alone() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 0..600 {
        log.append(&record(n, n)).unwrap();
    }
    log.index_tail(true).unwrap();
    damage(&path, 100);
    fs::remove_dir_all(dir.path().join("log.idx")).unwrap();

    for n in 600..900 {
        log.append(&record(n, n)).unwrap();
    }
    log.sync_from_disk().unwrap();
    let before = len(&path);
    assert!(corrupt(log.index_tail(true)), "the damage is reported");
    assert_eq!(
        len(&path),
        before,
        "building an index never shortens the log"
    );
    // the handle answers as it did before the failed attempt
    for n in [0, 99, 101, 599, 600, 899] {
        assert_eq!(value(&log, n), Some(n), "record {n}");
    }
}

#[test]
fn catching_up_cuts_a_torn_last_record_and_nothing_before_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (mut held, _) = RecLog::open(&path, LAYOUT).unwrap();
    held.append(&record(0, 0)).unwrap();

    // another writer appends 20 records and dies in the last one
    let (mut other, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 1..=20 {
        other.append(&record(n, n)).unwrap();
    }
    drop(other);
    damage(&path, 20);
    held.sync_from_disk().unwrap();
    assert_eq!(held.count(), 20);
    assert_eq!(len(&path), 5 + 20 * REC_LEN as u64);
    assert_eq!(value(&held, 19), Some(19));

    // damage that is not at the end is not a torn append
    let (mut other, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 20..40 {
        other.append(&record(n, n)).unwrap();
    }
    drop(other);
    damage(&path, 25);
    let before = len(&path);
    assert!(corrupt(held.sync_from_disk()), "the damage is reported");
    assert_eq!(len(&path), before, "the records after it stay");
    assert_eq!(value(&held, 19), Some(19));
}

/// Where record `n` of an active pack starts: a 5-byte file header, then
/// records of a 41-byte header whose last four bytes are the payload length.
fn pack_record(pack: &[u8], n: usize) -> u64 {
    let mut at = 5usize;
    for _ in 0..n {
        let stored = u32::from_le_bytes(pack[at + 37..at + 41].try_into().unwrap());
        at += 41 + stored as usize;
    }
    at as u64
}

#[test]
fn a_chunk_rewind_over_a_damaged_record_header_cuts_only_what_it_was_asked_to() {
    let dir = alt_os::temp::tempdir().unwrap();
    let pack = dir.path().join("pack-00000001.altpack");
    let mut store = ChunkStore::open_with(dir.path(), Options::default()).unwrap();
    for n in 0..600 {
        store.put(&chunk(n)).unwrap();
    }
    store.flush().unwrap();
    store.index_tail().unwrap();
    drop(store);
    // the encoding byte of record 100, behind the runs: no record follows
    // one that claims an encoding there is none of
    let at = pack_record(&fs::read(&pack).unwrap(), 100) + 32;
    let file = OpenOptions::new().write(true).open(&pack).unwrap();
    file.write_all_at(&[0xff], at).unwrap();

    let mut store = ChunkStore::open_with(dir.path(), Options::default()).unwrap();
    let before = store.checkpoint();
    for n in 1000..1300 {
        store.put(&chunk(n)).unwrap();
    }
    store.flush().unwrap();
    store.index_tail().unwrap();
    let rewound = store.rewind(before);
    assert!(
        matches!(rewound, Err(StoreError::Format("active pack damaged"))),
        "the damage is reported"
    );
    assert!(
        len(&pack) > pack_record(&fs::read(&pack).unwrap(), 100) + 41,
        "the records after the damaged one stay"
    );
}
