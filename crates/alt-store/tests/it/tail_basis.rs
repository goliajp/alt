//! What is known of how far a log was synced decides whether a record that
//! does not read is an unfinished append, and holding the writers' lock
//! decides whether an unfinished append is cut off or passed over.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::time::SystemTime;

use alt_store::reclog::RecLog;
use alt_store::{Basis, ChunkStore, Options, StoreError, Tail};

use super::damaged_log::{len, pack_record};
use super::index_runs::{LAYOUT, REC_LEN, chunk, record, value};

const RECORDS: u64 = 20;
const VICTIM: u64 = 10;

fn write_at(path: &Path, at: u64, bytes: &[u8]) {
    let file = OpenOptions::new().write(true).open(path).unwrap();
    file.write_all_at(bytes, at).unwrap();
}

fn stamp(path: &Path) -> (Vec<u8>, SystemTime) {
    let modified = fs::metadata(path).unwrap().modified().unwrap();
    (fs::read(path).unwrap(), modified)
}

/// Every basis against a record at `at`, with whether it then is an
/// unfinished append.
fn bases(at: u64, last: bool) -> [(Basis, bool); 4] {
    [
        (Basis::Boundary(at), true),
        (Basis::Boundary(at + 1), false),
        (Basis::LastOnly, last),
        (Basis::Unknown, false),
    ]
}

fn tail(basis: Basis, locked: bool) -> Tail {
    let tail = Tail::judged_by(move |_, _| basis);
    if locked { tail.locked() } else { tail }
}

#[test]
fn a_pack_record_that_does_not_read_is_judged_by_the_basis_and_cut_only_under_the_lock() {
    for last in [true, false] {
        for locked in [true, false] {
            let dir = alt_os::temp::tempdir().unwrap();
            let pack = dir.path().join("pack-00000001.altpack");
            let mut store = ChunkStore::open(dir.path()).unwrap();
            let ids: Vec<_> = (0..RECORDS)
                .map(|n| store.put(&chunk(n)).unwrap())
                .collect();
            store.flush().unwrap();
            drop(store);
            let (at, kept) = if last {
                // less than a record header past the last record
                let whole = len(&pack);
                write_at(&pack, whole, &[0xcd; 20]);
                (whole, RECORDS)
            } else {
                // an encoding there is none of, whole records after it
                let at = pack_record(&fs::read(&pack).unwrap(), VICTIM as usize);
                write_at(&pack, at + 32, &[0xff]);
                (at, VICTIM)
            };
            let mut before = stamp(&pack);
            for (basis, unfinished) in bases(at, last) {
                let case = format!("last {last}, locked {locked}, {basis:?}");
                let opened = ChunkStore::open_known(
                    dir.path(),
                    Options::default(),
                    None,
                    tail(basis, locked),
                );
                if !unfinished {
                    let damaged = matches!(opened, Err(StoreError::Format("active pack damaged")));
                    assert!(damaged, "{case}");
                    assert!(stamp(&pack) == before, "{case}");
                    continue;
                }
                let store = opened.unwrap();
                for (n, id) in ids.iter().enumerate() {
                    assert_eq!(store.contains(*id).unwrap(), (n as u64) < kept, "{case}");
                }
                if locked {
                    assert_eq!(len(&pack), at, "{case}");
                    // put back what the cut took for the next basis
                    drop(store);
                    fs::write(&pack, &before.0).unwrap();
                    before = stamp(&pack);
                } else {
                    assert!(stamp(&pack) == before, "{case}");
                }
            }
        }
    }
}

#[test]
fn a_log_record_that_does_not_read_is_judged_by_the_basis_and_cut_only_under_the_lock() {
    for last in [true, false] {
        for locked in [true, false] {
            let dir = alt_os::temp::tempdir().unwrap();
            let path = dir.path().join("log");
            let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
            for n in 0..RECORDS {
                log.append(&record(n, n)).unwrap();
            }
            drop(log);
            let (at, kept) = if last {
                let whole = len(&path);
                write_at(&path, whole, &[0xee; 30]);
                (whole, RECORDS)
            } else {
                let at = 5 + VICTIM * REC_LEN as u64;
                write_at(&path, at + 33, &[0xff]);
                (at, VICTIM)
            };
            let mut before = stamp(&path);
            for (basis, unfinished) in bases(at, last) {
                let case = format!("last {last}, locked {locked}, {basis:?}");
                let opened = RecLog::open_with_runs(&path, LAYOUT, None, tail(basis, locked));
                if !unfinished {
                    let corrupt =
                        matches!(opened, Err(StoreError::Format(what)) if what == LAYOUT.corrupt);
                    assert!(corrupt, "{case}");
                    assert!(stamp(&path) == before, "{case}");
                    continue;
                }
                let (log, _) = opened.unwrap();
                assert_eq!(log.count() as u64, kept, "{case}");
                assert_eq!(value(&log, kept - 1), Some(kept - 1), "{case}");
                assert_eq!(value(&log, kept), None, "{case}");
                if locked {
                    assert_eq!(len(&path), at, "{case}");
                    drop(log);
                    fs::write(&path, &before.0).unwrap();
                    before = stamp(&path);
                } else {
                    assert!(stamp(&path) == before, "{case}");
                }
            }
        }
    }
}

#[test]
fn a_handle_that_passed_over_an_unfinished_end_cuts_it_off_when_it_catches_up_to_write() {
    let dir = alt_os::temp::tempdir().unwrap();
    let pack = dir.path().join("pack-00000001.altpack");
    let mut store = ChunkStore::open(dir.path()).unwrap();
    for n in 0..RECORDS {
        store.put(&chunk(n)).unwrap();
    }
    store.flush().unwrap();
    drop(store);
    let whole = len(&pack);
    write_at(&pack, whole, &[0xcd; 20]);
    let reader = tail(Basis::LastOnly, false);
    let mut store = ChunkStore::open_known(dir.path(), Options::default(), None, reader).unwrap();
    assert_eq!(len(&pack), whole + 20);
    // the writers' lock is taken here
    store.sync_from_disk().unwrap();
    assert_eq!(len(&pack), whole);
    let id = store.put(&chunk(RECORDS)).unwrap();
    store.flush().unwrap();
    drop(store);
    let store = ChunkStore::open(dir.path()).unwrap();
    assert_eq!(store.get(id).unwrap(), chunk(RECORDS));

    let path = dir.path().join("log");
    let (mut log, _) = RecLog::open(&path, LAYOUT).unwrap();
    for n in 0..RECORDS {
        log.append(&record(n, n)).unwrap();
    }
    drop(log);
    let whole = len(&path);
    write_at(&path, whole, &[0xee; 30]);
    let reader = tail(Basis::LastOnly, false);
    let (mut log, _) = RecLog::open_with_runs(&path, LAYOUT, None, reader).unwrap();
    assert_eq!(len(&path), whole + 30);
    log.sync_from_disk().unwrap();
    assert_eq!(len(&path), whole);
    log.append(&record(RECORDS, 7)).unwrap();
    drop(log);
    let (log, _) = RecLog::open(&path, LAYOUT).unwrap();
    assert_eq!(value(&log, RECORDS), Some(7));
    drop(log);

    // a log cut short inside its header is given it by the writer only
    let header = fs::read(&path).unwrap();
    fs::write(&path, &header[..2]).unwrap();
    fs::remove_dir_all(dir.path().join("log.idx")).ok();
    let reader = tail(Basis::LastOnly, false);
    let (mut log, _) = RecLog::open_with_runs(&path, LAYOUT, None, reader).unwrap();
    assert_eq!((log.count(), len(&path)), (0, 2));
    log.sync_from_disk().unwrap();
    log.append(&record(1, 1)).unwrap();
    drop(log);
    let (log, _) = RecLog::open(&path, LAYOUT).unwrap();
    assert_eq!((log.count(), value(&log, 1)), (1, Some(1)));
}
