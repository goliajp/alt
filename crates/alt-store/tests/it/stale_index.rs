//! A log changed behind its index runs by a program that does not know
//! about them (an older alt cutting a failed batch off and writing another
//! one): the runs must not be taken for an index of the new content, and
//! where they are, no write may be skipped on their word.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;

use alt_store::reclog::RecLog;
use alt_store::{ChunkId, ChunkStore, Options, StoreError};

use super::index_runs::{LAYOUT, REC_LEN, chunk, record, value};

/// What an older program does: it never touches the index directory.
fn rewrite_tail(path: &std::path::Path, keep: u64, records: impl Iterator<Item = [u8; REC_LEN]>) {
    let file = OpenOptions::new().write(true).open(path).unwrap();
    let mut at = 5 + keep * REC_LEN as u64;
    file.set_len(at).unwrap();
    for rec in records {
        file.write_all_at(&rec, at).unwrap();
        at += REC_LEN as u64;
    }
}

#[test]
fn a_log_regrown_to_the_same_length_and_last_record_is_not_taken_for_the_old_one() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 0..600 {
        log.append(&record(n, n)).unwrap();
    }
    log.index_tail(u64::MAX, true).unwrap();
    assert!(!log.index_pending());
    drop(log);

    // records 100..599 replaced by others; the last one is the same record
    // in the same place, so the bytes just before the end are unchanged
    let regrown = (100..599)
        .map(|n| record(n + 10_000, n))
        .chain([record(599, 599)]);
    rewrite_tail(&path, 100, regrown);

    let (log, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in (0..100).chain([599]) {
        assert_eq!(value(&log, n), Some(n), "record {n}");
    }
    for n in [100, 350, 598] {
        assert_eq!(value(&log, n), None, "record {n} is gone");
        assert_eq!(value(&log, n + 10_000), Some(n), "its replacement is found");
    }
    assert_eq!(log.check(|_| Ok::<_, StoreError>(())).unwrap(), 600);
}

/// One raw record as the pack holds it.
fn raw_record(data: &[u8]) -> Vec<u8> {
    let mut rec = ChunkId::of(data).0.to_vec();
    rec.push(0);
    rec.extend_from_slice(&(data.len() as u32).to_le_bytes());
    rec.extend_from_slice(&(data.len() as u32).to_le_bytes());
    rec.extend_from_slice(data);
    rec
}

#[test]
fn a_chunk_the_runs_wrongly_list_is_written_not_skipped() {
    let dir = alt_os::temp::tempdir().unwrap();
    // stored raw, so every record has the same size
    let opts = Options {
        raw_threshold: usize::MAX,
        ..Options::default()
    };
    let mut store = ChunkStore::open_with(dir.path(), opts).unwrap();
    let ids: Vec<ChunkId> = (0..600).map(|n| store.put(&chunk(n)).unwrap()).collect();
    store.flush().unwrap();
    store.index_tail().unwrap();
    assert!(!store.index_pending());
    drop(store);

    // record 100 becomes another chunk of the same size, far enough from
    // the end that no guard over the last bytes can tell
    let record_len = raw_record(&chunk(0)).len() as u64;
    let pack = OpenOptions::new()
        .write(true)
        .open(dir.path().join("pack-00000001.altpack"))
        .unwrap();
    pack.write_all_at(&raw_record(&chunk(9_999)), 5 + 100 * record_len)
        .unwrap();
    drop(pack);

    // a reader is told the index is wrong rather than that the chunk exists
    let reader = ChunkStore::open_with(dir.path(), opts).unwrap();
    assert!(reader.contains(ids[100]).is_err());
    assert!(reader.contains(ids[99]).unwrap());
    drop(reader);

    // a writer stores the chunk: skipping it would lose the data
    let mut store = ChunkStore::open_with(dir.path(), opts).unwrap();
    store.sync_from_disk().unwrap();
    assert_eq!(store.put(&chunk(100)).unwrap(), ids[100]);
    store.flush().unwrap();
    store.index_tail().unwrap();
    drop(store);

    let store = ChunkStore::open_with(dir.path(), opts).unwrap();
    assert_eq!(store.get(ids[100]).unwrap(), chunk(100));
    assert_eq!(store.get(ChunkId::of(&chunk(9_999))).unwrap(), chunk(9_999));
    assert_eq!(store.get(ids[599]).unwrap(), chunk(599));
    assert_eq!(store.check().unwrap(), 601);
    assert!(fs::read_dir(dir.path().join("chunks.idx")).unwrap().count() >= 1);
}
