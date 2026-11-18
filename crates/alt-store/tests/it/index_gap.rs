//! A handle that stays open while the index runs on disk fall behind it —
//! their directory removed, or another handle cutting the log back — must
//! not write a run that claims records it holds no entries for.

use std::fs;

use alt_store::reclog::RecLog;
use alt_store::{ChunkStore, Options};

use super::index_runs::{LAYOUT, chunk, record, value};

#[test]
fn record_log_indexes_everything_after_its_index_dir_is_removed() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 0..300 {
        log.append(&record(n, n)).unwrap();
    }
    log.index_tail(u64::MAX, true).unwrap();
    assert!(!log.index_pending(), "the first 300 are in a run");

    fs::remove_dir_all(dir.path().join("log.idx")).unwrap();
    for n in 300..600 {
        log.append(&record(n, n)).unwrap();
    }
    log.sync_from_disk().unwrap();
    log.index_tail(u64::MAX, true).unwrap();
    for n in [0, 150, 299, 300, 599] {
        assert_eq!(
            value(&log, n),
            Some(n),
            "record {n} through the open handle"
        );
    }
    drop(log);

    let (log, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 0..600 {
        assert_eq!(value(&log, n), Some(n), "record {n} after reopening");
    }
    assert_eq!(
        log.check(|_| Ok::<_, alt_store::StoreError>(())).unwrap(),
        600
    );
}

#[test]
fn record_log_follows_another_handle_cutting_the_log_back() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (mut held, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 0..600 {
        held.append(&record(n, n)).unwrap();
    }
    held.index_tail(u64::MAX, true).unwrap();

    // another handle drops records 100.. and writes different ones
    let (mut other, _) = RecLog::open(&path, LAYOUT).unwrap();
    let cut = other.appended_len() - 500 * record(0, 0).len() as u64;
    other.rewind(cut).unwrap();
    for n in 1000..1250 {
        other.append(&record(n, n)).unwrap();
    }
    drop(other);

    held.sync_from_disk().unwrap();
    for n in 2000..2300 {
        held.append(&record(n, n)).unwrap();
    }
    held.index_tail(u64::MAX, true).unwrap();
    for n in [50, 1100, 2100] {
        assert_eq!(
            value(&held, n),
            Some(n),
            "record {n} through the held handle"
        );
    }
    assert_eq!(value(&held, 350), None);
    drop(held);

    let (log, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in (0..100).chain(1000..1250).chain(2000..2300) {
        assert_eq!(value(&log, n), Some(n), "record {n}");
    }
    for n in [100, 350, 599] {
        assert_eq!(value(&log, n), None, "record {n} was cut off");
    }
    assert_eq!(
        log.check(|_| Ok::<_, alt_store::StoreError>(())).unwrap(),
        650
    );
}

#[test]
fn chunk_store_indexes_everything_after_its_index_dir_is_removed() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut store = ChunkStore::open_with(dir.path(), Options::default()).unwrap();
    let mut ids: Vec<_> = (0..300).map(|n| store.put(&chunk(n)).unwrap()).collect();
    store.flush().unwrap();
    store.index_tail().unwrap();
    assert!(!store.index_pending(), "the first 300 are in a run");

    fs::remove_dir_all(dir.path().join("chunks.idx")).unwrap();
    ids.extend((300..600).map(|n| store.put(&chunk(n)).unwrap()));
    store.flush().unwrap();
    store.sync_from_disk().unwrap();
    store.index_tail().unwrap();
    drop(store);

    let mut store = ChunkStore::open_with(dir.path(), Options::default()).unwrap();
    for (n, id) in ids.iter().enumerate() {
        assert_eq!(store.get(*id).unwrap(), chunk(n as u64), "chunk {n}");
    }
    assert_eq!(store.check().unwrap(), 600);
    // a chunk the index lost would be written a second time
    store.put(&chunk(7)).unwrap();
    store.flush().unwrap();
    assert_eq!(store.check().unwrap(), 600);
}

#[test]
fn a_run_temporary_file_left_by_a_crash_is_removed_by_the_next_writer() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 0..300 {
        log.append(&record(n, n)).unwrap();
    }
    log.index_tail(u64::MAX, true).unwrap();
    drop(log);

    let leftover = dir.path().join("log.idx/000000000005-00000000ffff.run.tmp");
    fs::write(&leftover, b"half a run").unwrap();
    // a reader leaves it: only a writer, under the lock, knows it is dead
    let (log, _) = RecLog::open(&path, LAYOUT).unwrap();
    assert_eq!(value(&log, 7), Some(7));
    drop(log);
    assert!(leftover.exists());

    let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
    log.sync_from_disk().unwrap();
    log.append(&record(300, 300)).unwrap();
    assert!(!leftover.exists(), "the leftover is gone");
    assert_eq!(value(&log, 300), Some(300));
}

#[test]
fn a_lookup_repeated_after_the_log_changed_sees_the_change() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 0..300 {
        log.append(&record(n, n)).unwrap();
    }
    log.index_tail(u64::MAX, true).unwrap();
    // records read from behind the runs are kept from now on: the second
    // pass is answered from memory
    log.keep_reads();
    for _ in 0..2 {
        for n in 0..300 {
            assert_eq!(value(&log, n), Some(n));
        }
    }

    // the key written again: the newer record wins while it is in the
    // tail, and after it has moved behind the runs
    for n in 300..600 {
        log.append(&record(if n == 300 { 7 } else { n }, n))
            .unwrap();
    }
    assert_eq!(value(&log, 7), Some(300));
    log.index_tail(u64::MAX, true).unwrap();
    assert_eq!(value(&log, 7), Some(300));
    assert_eq!(value(&log, 250), Some(250));

    // cut back to the first 200 records: what was kept past them is gone
    let cut = log.appended_len() - 400 * record(0, 0).len() as u64;
    log.rewind(cut).unwrap();
    assert_eq!(value(&log, 7), Some(7));
    assert_eq!(value(&log, 250), None);
    assert_eq!(value(&log, 199), Some(199));
}

/// A rewind drops every run that reaches past its target, also one this
/// handle never loaded: another handle indexed the log after this one
/// opened it.
#[test]
fn a_rewind_drops_the_runs_another_handle_wrote_past_its_target() {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    let (mut held, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 0..10 {
        held.append(&record(n, n)).unwrap();
    }
    let kept = held.appended_len();

    let (mut other, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 10..400 {
        other.append(&record(n, n)).unwrap();
    }
    other.index_tail(u64::MAX, true).unwrap();
    drop(other);
    let runs = || {
        std::fs::read_dir(dir.path().join("log.idx"))
            .unwrap()
            .count()
    };
    assert_eq!(runs(), 1);

    // the handle that knows of no run cuts the log back behind that one
    held.sync_from_disk().unwrap();
    held.rewind(kept).unwrap();
    assert_eq!(runs(), 0, "a run is left over records that are gone");
    for n in 400..700 {
        held.append(&record(n, n)).unwrap();
    }
    drop(held);
    let (log, _) = RecLog::open(&path, LAYOUT).unwrap();
    assert_eq!(value(&log, 5), Some(5));
    assert_eq!(value(&log, 200), None);
    assert_eq!(value(&log, 650), Some(650));
}
