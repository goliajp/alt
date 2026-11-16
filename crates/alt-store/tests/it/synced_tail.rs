//! A record that does not read is damage when it starts before the length
//! the log was synced to, and an append that never finished past it. Damage
//! is reported with the file left as it is, wherever in the file it sits
//! and whether or not an index covers it.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_store::reclog::RecLog;
use alt_store::{ChunkId, ChunkStore, Options, StoreError, Tail};

use super::damaged_log::{len, pack_record};
use super::index_runs::{LAYOUT, REC_LEN, chunk, record, value};
use super::synced;

fn write_at(path: &Path, at: u64, bytes: &[u8]) {
    let file = OpenOptions::new().write(true).open(path).unwrap();
    file.write_all_at(bytes, at).unwrap();
}

/// A store of `indexed` chunks behind index runs and `tail` more past them.
fn chunks(dir: &Path, indexed: u64, tail: u64) -> Vec<ChunkId> {
    let mut store = ChunkStore::open(dir).unwrap();
    let mut ids: Vec<_> = (0..indexed)
        .map(|n| store.put(&chunk(n)).unwrap())
        .collect();
    store.flush().unwrap();
    store.index_tail().unwrap();
    ids.extend((indexed..indexed + tail).map(|n| store.put(&chunk(n)).unwrap()));
    store.flush().unwrap();
    ids
}

fn open_chunks(dir: &Path, tail: Tail) -> Result<ChunkStore, StoreError> {
    ChunkStore::open_known(dir, Options::default(), None, tail)
}

#[test]
fn a_damaged_record_header_in_a_synced_pack_is_reported_and_the_pack_left_alone() {
    // (indexed, past the runs, damaged record, offset in its header, byte, runs removed)
    let cases = [
        (600, 0, 100, 32, 0xff, true), // encoding there is none of
        (600, 0, 100, 40, 0x7f, true), // length made larger
        (600, 100, 650, 32, 0xff, false),
        (600, 100, 650, 40, 0x7f, false),
        (600, 100, 650, 37, 0x01, false), // length made smaller
        (600, 100, 699, 40, 0x7f, false), // the last record
    ];
    for (indexed, tail, victim, field, byte, remove_runs) in cases {
        let dir = alt_os::temp::tempdir().unwrap();
        let pack = dir.path().join("pack-00000001.altpack");
        chunks(dir.path(), indexed, tail);
        let at = pack_record(&fs::read(&pack).unwrap(), victim) + field;
        write_at(&pack, at, &[byte]);
        if remove_runs {
            fs::remove_dir_all(dir.path().join("chunks.idx")).unwrap();
        }
        let before = fs::read(&pack).unwrap();
        let opened = open_chunks(dir.path(), synced(before.len() as u64));
        assert!(
            matches!(opened, Err(StoreError::Format("active pack damaged"))),
            "record {victim}, header byte {field}"
        );
        assert!(fs::read(&pack).unwrap() == before, "the pack is as it was");
    }
}

#[test]
fn what_does_not_read_past_the_synced_length_of_a_pack_is_cut_off() {
    let dir = alt_os::temp::tempdir().unwrap();
    let pack = dir.path().join("pack-00000001.altpack");
    let ids = chunks(dir.path(), 600, 100);
    let bytes = fs::read(&pack).unwrap();
    let (synced_len, damaged) = (pack_record(&bytes, 640), pack_record(&bytes, 650));
    write_at(&pack, damaged + 32, &[0xff]);

    let store = open_chunks(dir.path(), synced(synced_len)).unwrap();
    assert_eq!(len(&pack), damaged, "cut where it stops reading");
    assert!(store.contains(ids[649]).unwrap());
    assert!(!store.contains(ids[650]).unwrap());
}

#[test]
fn nothing_is_cut_from_a_pack_whose_synced_length_is_not_known() {
    let dir = alt_os::temp::tempdir().unwrap();
    let pack = dir.path().join("pack-00000001.altpack");
    chunks(dir.path(), 0, 20);
    let whole = len(&pack);
    write_at(&pack, whole, &[0xcd; 20]);
    for tail in [Tail::default(), Tail::synced_to(|_| None)] {
        let opened = open_chunks(dir.path(), tail);
        assert!(matches!(
            opened,
            Err(StoreError::Format("active pack damaged"))
        ));
        assert_eq!(len(&pack), whole + 20);
    }
}

#[test]
fn a_read_only_open_passes_over_an_unfinished_end_and_changes_nothing() {
    let dir = alt_os::temp::tempdir().unwrap();
    let pack = dir.path().join("pack-00000001.altpack");
    let ids = chunks(dir.path(), 0, 20);
    let whole = len(&pack);
    write_at(&pack, whole, &[0xcd; 20]);
    let log = dir.path().join("log");
    let (mut written, _) = RecLog::open(&log, LAYOUT).unwrap();
    for n in 0..20 {
        written.append(&record(n, n)).unwrap();
    }
    drop(written);
    write_at(&log, len(&log), &[0xee; 30]);

    let store = open_chunks(dir.path(), synced(whole).read_only()).unwrap();
    assert!(store.contains(ids[19]).unwrap());
    assert_eq!(len(&pack), whole + 20);
    let tail = synced(5).read_only();
    let (read, _) = RecLog::open_with_runs(&log, LAYOUT, None, tail).unwrap();
    assert_eq!(read.count(), 20);
    assert_eq!(value(&read, 19), Some(19));
    assert_eq!(len(&log), 5 + 20 * REC_LEN as u64 + 30);

    // and it creates nothing that is not there
    let missing = dir.path().join("none");
    let tail = synced(5).read_only();
    assert!(RecLog::open_with_runs(&missing.join("log"), LAYOUT, None, tail).is_err());
    assert!(open_chunks(&missing, synced(5).read_only()).is_err());
    assert!(!missing.exists());
}

fn log_of(dir: &Path, records: u64) -> std::path::PathBuf {
    let path = dir.join("log");
    let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 0..records {
        log.append(&record(n, n)).unwrap();
    }
    path
}

fn open_log(path: &Path, tail: Tail) -> Result<RecLog, StoreError> {
    RecLog::open_with_runs(path, LAYOUT, None, tail).map(|(log, _)| log)
}

#[test]
fn a_damaged_last_record_of_a_synced_log_is_reported() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = log_of(dir.path(), 20);
    write_at(&path, 5 + 19 * REC_LEN as u64 + 33, &[0xff]);
    let whole = len(&path);
    assert!(open_log(&path, synced(whole)).is_err());
    assert!(open_log(&path, Tail::default()).is_err());
    assert_eq!(len(&path), whole);
}

#[test]
fn every_record_that_does_not_read_past_the_synced_length_of_a_log_is_cut_off() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = log_of(dir.path(), 20);
    // power went out with the last three records not on disk
    write_at(&path, 5 + 17 * REC_LEN as u64, &[0u8; 3 * REC_LEN]);
    let log = open_log(&path, synced(5 + 15 * REC_LEN as u64)).unwrap();
    assert_eq!(log.count(), 17);
    assert_eq!(len(&path), 5 + 17 * REC_LEN as u64);
}

#[test]
fn damage_in_a_synced_log_whose_index_was_removed_is_reported() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 0..600 {
        log.append(&record(n, n)).unwrap();
    }
    log.index_tail(true).unwrap();
    drop(log);
    write_at(&path, 5 + 100 * REC_LEN as u64 + 33, &[0xff]);
    fs::remove_dir_all(dir.path().join("log.idx")).unwrap();
    let whole = len(&path);
    assert!(open_log(&path, synced(whole)).is_err());
    assert_eq!(len(&path), whole);
}
