//! Index runs: lookups through them, merging, the guard against a log that
//! was cut back, and the stores opening the same with or without them.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use alt_store::reclog::{self, INDEX_AT, Layout, RecLog};
use alt_store::runs::{Guard, Key, Runs};
use alt_store::{BlobStore, ChunkId, ChunkStore, Options, StoreError};

pub(crate) fn key(n: u64) -> Key {
    *alt_hash::blake3::hash(&n.to_le_bytes()).as_bytes()
}

fn guard(n: u64) -> Guard {
    key(n)[..16].to_vec()
}

/// Opens runs over a pretend log whose guard at `end` is `guard(end)` up to
/// `log_len`.
fn open_runs(dir: &Path, log_len: u64) -> Runs {
    Runs::open(dir, 0, |end| (end <= log_len).then(|| guard(end))).unwrap()
}

fn positions(runs: &Runs, k: &Key) -> Vec<u64> {
    let mut out = Vec::new();
    runs.find_each(k, |p| out.push(p)).unwrap();
    out
}

#[test]
fn runs_find_what_was_indexed_across_merges_and_reopen() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut runs = open_runs(dir.path(), 0);
    let mut end = 0u64;
    // batches of very different sizes, so some merge and some do not
    for batch in [70_000u64, 300, 300, 5, 9_000, 1, 1, 1] {
        let entries: Vec<(Key, u64)> = (end..end + batch).map(|n| (key(n), n)).collect();
        end += batch;
        runs.extend(&entries, end - batch, end, guard(end), true)
            .unwrap();
    }
    assert_eq!(runs.covered(), end);
    assert_eq!(runs.count() as u64, end);
    assert!(runs.run_count() < 8, "smaller neighbours are merged");
    runs.verify().unwrap();

    let reopened = open_runs(dir.path(), end);
    assert_eq!(reopened.covered(), end);
    for n in [0, 1, 69_999, 70_000, 70_299, 79_604, end - 1] {
        assert_eq!(reopened.find_last(&key(n)).unwrap(), Some(n));
    }
    assert_eq!(reopened.find_last(&key(end)).unwrap(), None);
    let files = fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(files, reopened.run_count(), "merged runs are deleted");
}

#[test]
fn a_key_indexed_twice_reports_every_position_and_the_latest() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut runs = open_runs(dir.path(), 0);
    runs.extend(
        &[(key(1), 10), (key(2), 20), (key(1), 30)],
        0,
        40,
        guard(40),
        false,
    )
    .unwrap();
    runs.extend(&[(key(1), 50)], 40, 60, guard(60), false)
        .unwrap();
    assert_eq!(positions(&runs, &key(1)), vec![10, 30, 50]);
    assert_eq!(runs.find_last(&key(1)).unwrap(), Some(50));
    assert_eq!(runs.find_last(&key(2)).unwrap(), Some(20));

    let (lo, hi) = ([0u8; 32], [0xffu8; 32]);
    let mut seen = 0;
    runs.range(&lo, &hi, |_, _| {
        seen += 1;
        true
    })
    .unwrap();
    assert_eq!(seen, 4);
}

#[test]
fn runs_past_a_cut_back_log_are_dropped_and_then_removed() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut runs = open_runs(dir.path(), 0);
    let first: Vec<(Key, u64)> = (0..5_000).map(|n| (key(n), n)).collect();
    runs.extend(&first, 0, 5_000, guard(5_000), false).unwrap();
    runs.extend(&[(key(5_000), 5_000)], 5_000, 5_001, guard(5_001), false)
        .unwrap();
    assert_eq!(runs.run_count(), 2);

    // the log now ends before the second run's end
    let mut shorter = open_runs(dir.path(), 5_000);
    assert_eq!(shorter.covered(), 5_000);
    assert_eq!(shorter.find_last(&key(5_000)).unwrap(), None);
    assert_eq!(shorter.find_last(&key(4_999)).unwrap(), Some(4_999));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    shorter.remove_stale().unwrap();
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);

    // same length, different bytes before the end: the run does not apply
    let other = Runs::open(dir.path(), 0, |_| Some(vec![0u8; 16])).unwrap();
    assert_eq!(other.covered(), 0);

    let mut cut = open_runs(dir.path(), 5_000);
    cut.truncate(4_999).unwrap();
    assert_eq!(cut.covered(), 0);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn a_damaged_run_is_reported_or_ignored_never_trusted() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut runs = open_runs(dir.path(), 0);
    let entries: Vec<(Key, u64)> = (0..2_000).map(|n| (key(n), n)).collect();
    runs.extend(&entries, 0, 2_000, guard(2_000), false)
        .unwrap();
    drop(runs);
    let path = fs::read_dir(dir.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();

    // zero the first half of the entries: their keys leave their buckets
    let size = fs::metadata(&path).unwrap().len();
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(56)).unwrap();
    file.write_all(&vec![0u8; 1_000 * 40]).unwrap();
    drop(file);
    let runs = open_runs(dir.path(), 2_000);
    assert!(runs.verify().is_err());
    let failed = (0..2_000)
        .filter(|n| matches!(runs.find_last(&key(*n)), Err(StoreError::Format(_))))
        .count();
    assert!(failed > 900, "lookups into the damaged half fail: {failed}");
    drop(runs);

    // a damaged header or a short file is not a run at all
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(size - 1)
        .unwrap();
    assert_eq!(open_runs(dir.path(), 2_000).covered(), 0);
}

pub(crate) const REC_LEN: usize = 48;
pub(crate) const LAYOUT: Layout = Layout {
    magic: *b"TEST",
    version: 1,
    rec_len: REC_LEN,
    bad_header: "bad test header",
    bad_version: "unsupported test version",
    corrupt: "test record corrupt",
    bad_rewind: "bad test rewind",
    keys: |rec| (rec[..32].try_into().unwrap(), None),
};

pub(crate) fn record(n: u64, value: u64) -> [u8; REC_LEN] {
    let mut rec = [0u8; REC_LEN];
    rec[..32].copy_from_slice(&key(n));
    rec[32..40].copy_from_slice(&value.to_le_bytes());
    let check = reclog::checksum(&rec[..40]);
    rec[40..].copy_from_slice(&check);
    rec
}

pub(crate) fn value(log: &RecLog, n: u64) -> Option<u64> {
    log.get(&key(n))
        .unwrap()
        .map(|rec| u64::from_le_bytes(rec[32..40].try_into().unwrap()))
}

fn fill(path: &Path, range: std::ops::Range<u64>, index: bool) {
    let (mut log, _) = RecLog::open(path, LAYOUT).unwrap();
    for n in range {
        log.append(&record(n, n * 2)).unwrap();
    }
    if index {
        log.index_tail(true).unwrap();
    }
}

#[test]
fn record_log_reads_the_same_with_and_without_its_index() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let idx = dir.path().join("log.idx");
    let total = INDEX_AT as u64 * 3;
    fill(&path, 0..total, true);
    assert!(idx.is_dir(), "a long tail is indexed");
    fill(&path, total..total + 10, true);
    {
        let mut log = RecLog::open(&path, LAYOUT).unwrap().0;
        // a later record for the same key wins, in the tail and after indexing
        log.append(&record(7, 99)).unwrap();
        assert_eq!(value(&log, 7), Some(99));
    }

    let check = |log: &RecLog| {
        assert_eq!(log.count() as u64, total + 11);
        assert_eq!(value(log, 0), Some(0));
        assert_eq!(value(log, 7), Some(99));
        assert_eq!(value(log, total - 1), Some((total - 1) * 2));
        assert_eq!(value(log, total + 9), Some((total + 9) * 2));
        assert_eq!(value(log, total + 10), None);
        let mut seen = 0u64;
        log.for_each(|rec| -> Result<(), StoreError> {
            let expect = if seen == total + 10 {
                key(7)
            } else {
                key(seen)
            };
            assert_eq!(rec[..32], expect);
            seen += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, total + 11);
    };
    check(&RecLog::open(&path, LAYOUT).unwrap().0);
    // a log written before index runs existed has no index directory
    fs::remove_dir_all(&idx).unwrap();
    check(&RecLog::open(&path, LAYOUT).unwrap().0);
}

#[test]
fn record_log_detects_a_bad_record_behind_the_index_when_it_is_read() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    fill(&path, 0..INDEX_AT as u64, true);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let at = 5 + 3 * REC_LEN as u64 + 33;
    let mut byte = [0u8; 1];
    file.seek(SeekFrom::Start(at)).unwrap();
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Start(at)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    drop(file);

    let (log, _) = RecLog::open(&path, LAYOUT).unwrap();
    assert_eq!(value(&log, 2), Some(4));
    assert!(matches!(
        log.get(&key(3)),
        Err(StoreError::Format("test record corrupt"))
    ));
    let scan = log.for_each(|_| -> Result<(), StoreError> { Ok(()) });
    assert!(matches!(
        scan,
        Err(StoreError::Format("test record corrupt"))
    ));
}

#[test]
fn record_log_rewind_behind_the_index_keeps_the_rest_readable() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let total = INDEX_AT as u64 * 2;
    fill(&path, 0..total, true);
    let keep = 100u64;
    {
        let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
        log.rewind(5 + keep * REC_LEN as u64).unwrap();
        assert_eq!(value(&log, keep - 1), Some((keep - 1) * 2));
        assert_eq!(value(&log, keep), None);
        // regrow with different records at the same offsets
        for n in 0..INDEX_AT as u64 {
            log.append(&record(1_000_000 + n, n)).unwrap();
        }
        log.index_tail(true).unwrap();
    }
    let (log, _) = RecLog::open(&path, LAYOUT).unwrap();
    assert_eq!(log.count() as u64, keep + INDEX_AT as u64);
    assert_eq!(value(&log, keep - 1), Some((keep - 1) * 2));
    assert_eq!(value(&log, keep), None);
    assert_eq!(value(&log, 1_000_005), Some(5));
}

#[test]
fn record_log_torn_tail_after_the_index_is_cut_off() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    fill(&path, 0..INDEX_AT as u64, true);
    fill(&path, 1_000..1_003, false);
    let size = fs::metadata(&path).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(size - 7)
        .unwrap();
    // synced when it was indexed, and no further
    let synced = super::synced(5 + (INDEX_AT * REC_LEN) as u64);
    let (log, _) = RecLog::open_with_runs(&path, LAYOUT, None, synced).unwrap();
    assert_eq!(log.count(), INDEX_AT + 2);
    assert_eq!(value(&log, 1_001), Some(2_002));
    assert_eq!(value(&log, 1_002), None);
    assert_eq!(fs::metadata(&path).unwrap().len(), size - REC_LEN as u64);

    // a log cut back behind its index: the index no longer applies
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(5 + 10 * REC_LEN as u64)
        .unwrap();
    let (log, _) = RecLog::open(&path, LAYOUT).unwrap();
    assert_eq!(log.count(), 10);
    assert_eq!(value(&log, 9), Some(18));
    assert_eq!(value(&log, 10), None);
}

pub(crate) fn chunk(n: u64) -> Vec<u8> {
    let mut data = key(n).to_vec();
    data.extend_from_slice(&[n as u8; 100]);
    data
}

fn fill_chunks(dir: &Path, opts: Options, range: std::ops::Range<u64>) -> Vec<ChunkId> {
    let mut store = ChunkStore::open_with(dir, opts).unwrap();
    let ids = range.map(|n| store.put(&chunk(n)).unwrap()).collect();
    store.flush().unwrap();
    store.index_tail().unwrap();
    ids
}

#[test]
fn chunk_store_reads_the_same_with_and_without_its_index() {
    let dir = alt_os::temp::tempdir().unwrap();
    let total = INDEX_AT as u64 * 2;
    let mut ids = fill_chunks(dir.path(), Options::default(), 0..total);
    let idx = dir.path().join("chunks.idx");
    assert!(idx.is_dir());
    ids.extend(fill_chunks(
        dir.path(),
        Options::default(),
        total..total + 5,
    ));

    let check = |store: &ChunkStore| {
        assert_eq!(store.len() as u64, total + 5);
        for n in [0, 1, total - 1, total, total + 4] {
            assert_eq!(store.get(ids[n as usize]).unwrap(), chunk(n));
        }
        assert!(!store.contains(ChunkId::of(b"absent")).unwrap());
    };
    check(&ChunkStore::open(dir.path()).unwrap());
    fs::remove_dir_all(&idx).unwrap();
    check(&ChunkStore::open(dir.path()).unwrap());
}

#[test]
fn chunk_index_follows_seals_rewinds_deltas_and_compaction() {
    let dir = alt_os::temp::tempdir().unwrap();
    // small packs: the index has to span several of them
    let opts = Options {
        seal_threshold: 20_000,
        ..Options::default()
    };
    let total = INDEX_AT as u64 * 3;
    let ids = fill_chunks(dir.path(), opts, 0..total);
    {
        let store = ChunkStore::open_with(dir.path(), opts).unwrap();
        for n in (0..total).step_by(37) {
            assert_eq!(store.get(ids[n as usize]).unwrap(), chunk(n));
        }
    }

    // rewind: everything appended after the checkpoint goes, index included
    {
        let store = ChunkStore::open_with(dir.path(), opts).unwrap();
        let ckpt = store.checkpoint();
        let small = Options {
            seal_threshold: u64::MAX,
            ..opts
        };
        drop(store);
        let mut store = ChunkStore::open_with(dir.path(), small).unwrap();
        let extra: Vec<ChunkId> = (total..total + INDEX_AT as u64 + 3)
            .map(|n| store.put(&chunk(n)).unwrap())
            .collect();
        store.flush().unwrap();
        store.index_tail().unwrap();
        store.rewind(ckpt).unwrap();
        assert!(!store.contains(extra[0]).unwrap());
        assert_eq!(store.get(ids[5]).unwrap(), chunk(5));
    }
    let mut store = ChunkStore::open_with(dir.path(), opts).unwrap();
    assert_eq!(store.len() as u64, total);

    // a delta re-encoding supersedes the indexed location of its chunk
    let a = store.put(&vec![1u8; 9_000]).unwrap();
    let mut b_data = vec![1u8; 9_000];
    b_data[4_000] = 2;
    let b = store.put(&b_data).unwrap();
    store.flush().unwrap();
    store.reencode_as_delta(b, a).unwrap();
    store.flush().unwrap();
    drop(store);
    let mut store = ChunkStore::open_with(dir.path(), opts).unwrap();
    assert_eq!(store.get(b).unwrap(), b_data);

    store.compact().unwrap();
    assert_eq!(store.get(b).unwrap(), b_data);
    drop(store);
    let store = ChunkStore::open_with(dir.path(), opts).unwrap();
    assert_eq!(store.len() as u64, total + 2);
    assert_eq!(
        store.get(ids[total as usize - 1]).unwrap(),
        chunk(total - 1)
    );
    assert_eq!(store.get(b).unwrap(), b_data);
}

#[test]
fn blob_store_reopens_from_its_index() {
    let dir = alt_os::temp::tempdir().unwrap();
    let blobs: Vec<Vec<u8>> = (0..INDEX_AT as u64 * 2).map(chunk).collect();
    let ids: Vec<_> = {
        let mut store = BlobStore::open(dir.path()).unwrap();
        let ids = blobs
            .iter()
            .map(|b| store.put(b).unwrap())
            .collect::<Vec<_>>();
        store.flush().unwrap();
        store.index_tail().unwrap();
        ids
    };
    let store = BlobStore::open(dir.path()).unwrap();
    for (blob, id) in blobs.iter().zip(&ids).step_by(41) {
        assert_eq!(&store.get(*id).unwrap(), blob);
    }
}

/// Opening and looking up in a record log the size a repository of the
/// Linux kernel's order would have. Prints timings; asserts only results.
#[test]
#[ignore = "writes a log of ten million records (about 0.9 GB with its index)"]
fn record_log_at_ten_million_records() {
    use std::time::Instant;

    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let total = 10_000_000u64;
    // grown the way a repository grows: one large batch, then small ones
    let mut next = 0u64;
    // the last hundred stay in the tail: too few to index
    for batch in [total - 40_000, 30_000, 9_000, 700, 300, 100] {
        let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
        for n in next..next + batch {
            log.append(&record(n, n)).unwrap();
        }
        next += batch;
        let started = Instant::now();
        log.index_tail(true).unwrap();
        println!("indexed a batch of {batch} in {:?}", started.elapsed());
    }

    let mut opens = Vec::new();
    for _ in 0..21 {
        let started = Instant::now();
        let (log, _) = RecLog::open(&path, LAYOUT).unwrap();
        opens.push(started.elapsed());
        assert_eq!(log.count() as u64, total + 100);
    }
    opens.sort();
    println!("open: median {:?}, fastest {:?}", opens[10], opens[0]);

    let (log, _) = RecLog::open(&path, LAYOUT).unwrap();
    let started = Instant::now();
    let probes = 100_000u64;
    for i in 0..probes {
        let n = i * 99_991 % (total + 100);
        assert_eq!(value(&log, n), Some(n));
    }
    println!(
        "lookup of a present key: {:?} each",
        started.elapsed() / probes as u32
    );
    let started = Instant::now();
    for i in 0..probes {
        assert_eq!(value(&log, total + 1_000 + i), None);
    }
    println!(
        "lookup of an absent key: {:?} each",
        started.elapsed() / probes as u32
    );
    let runs = fs::read_dir(dir.path().join("log.idx")).unwrap().count();
    println!("index runs: {runs}");
}
