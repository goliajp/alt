//! Pack records a power cut left whole in structure and not in content: a
//! header with zeros or half a payload after it, or a stretch of zeros.
//! Past what is known synced they are appends that never finished; before
//! it they are damage, which the full check finds and nothing touches.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;

use alt_store::{Basis, ChunkId, ChunkStore, Options, StoreError, Tail};

use super::damaged_log::{len, pack_record};
use super::index_runs::chunk;

const RECORDS: u64 = 20;
const TORN: u64 = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Fault {
    /// the payloads of the last records are zeros
    Zeroed,
    /// the second half of the last record's payload is zeros
    Half,
    /// the last record, a delta, lost the second half of its payload
    HalfDelta,
    /// 4096 zero bytes follow the last record
    Zeros,
}

const FAULTS: [Fault; 4] = [Fault::Zeroed, Fault::Half, Fault::HalfDelta, Fault::Zeros];

fn noise(seed: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut word = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    while out.len() < 4000 {
        word ^= word << 13;
        word ^= word >> 7;
        word ^= word << 17;
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

fn zero(path: &Path, at: u64, len: u64) {
    let file = OpenOptions::new().write(true).open(path).unwrap();
    file.write_all_at(&vec![0u8; len as usize], at).unwrap();
}

/// A flushed store with `fault` in its pack. Returns the ids of the chunks
/// whose records are sound, those of the ones that are not, and where the
/// first record that is not begins.
fn torn(dir: &Path, fault: Fault) -> (Vec<ChunkId>, Vec<ChunkId>, u64) {
    let pack = dir.join("pack-00000001.altpack");
    let mut store = ChunkStore::open(dir).unwrap();
    let mut ids: Vec<_> = (0..RECORDS)
        .map(|n| store.put(&chunk(n)).unwrap())
        .collect();
    let mut records = RECORDS as usize;
    if fault == Fault::HalfDelta {
        let base = store.put(&noise(1)).unwrap();
        let mut near = noise(1);
        near[2000] ^= 0xff;
        let id = store.put(&near).unwrap();
        assert!(store.reencode_as_delta(id, base).unwrap());
        ids.push(base);
        // the chunk's first record, then the delta that replaces it
        records += 3;
    }
    store.flush().unwrap();
    drop(store);
    let data = fs::read(&pack).unwrap();
    let whole = len(&pack);
    let record = |n: usize| pack_record(&data, n);
    let payload = |n: usize| (record(n) + 41, record(n + 1) - record(n) - 41);
    match fault {
        Fault::Zeroed => {
            let first = records - TORN as usize;
            for n in first..records {
                let (at, len) = payload(n);
                zero(&pack, at, len);
            }
            let lost = ids.split_off(first);
            (ids, lost, record(first))
        }
        Fault::Half => {
            let (at, len) = payload(records - 1);
            zero(&pack, at + len / 2, len - len / 2);
            let lost = ids.split_off(records - 1);
            (ids, lost, record(records - 1))
        }
        Fault::HalfDelta => {
            let (at, len) = payload(records - 1);
            zero(&pack, at + len / 2, len - len / 2);
            // the chunk is still there in the record the delta replaced
            (ids, Vec::new(), record(records - 1))
        }
        Fault::Zeros => {
            zero(&pack, whole, 4096);
            (ids, vec![ChunkId([0; 32])], whole)
        }
    }
}

fn tail(basis: Basis, locked: bool) -> Tail {
    let tail = Tail::judged_by(move |_, _| basis);
    if locked { tail.locked() } else { tail }
}

fn open(dir: &Path, basis: Basis, locked: bool) -> Result<ChunkStore, StoreError> {
    ChunkStore::open_known(dir, Options::default(), None, tail(basis, locked))
}

/// The store holds `kept` and none of `lost`, and passes the full check.
fn holds(store: &ChunkStore, kept: &[ChunkId], lost: &[ChunkId], case: &str) {
    for id in kept {
        assert_eq!(ChunkId::of(&store.get(*id).unwrap()), *id, "{case}");
    }
    for id in lost {
        assert!(!store.contains(*id).unwrap(), "{case}");
    }
    store.check().unwrap_or_else(|e| panic!("{case}: {e:?}"));
}

#[test]
fn a_record_past_the_boundary_that_does_not_hold_what_it_names_never_finished() {
    for fault in FAULTS {
        for locked in [true, false] {
            let dir = alt_os::temp::tempdir().unwrap();
            let pack = dir.path().join("pack-00000001.altpack");
            let (kept, lost, at) = torn(dir.path(), fault);
            let before = fs::read(&pack).unwrap();
            let case = format!("{fault:?}, locked {locked}");
            let store = open(dir.path(), Basis::Boundary(at), locked).unwrap();
            holds(&store, &kept, &lost, &case);
            drop(store);
            if !locked {
                assert_eq!(fs::read(&pack).unwrap(), before, "{case}");
                continue;
            }
            assert_eq!(len(&pack), at, "{case}");
            // what was lost is written again and stays
            let mut store = open(dir.path(), Basis::Boundary(at), true).unwrap();
            if fault == Fault::Zeroed {
                for n in RECORDS - TORN..RECORDS {
                    assert_eq!(
                        store.put(&chunk(n)).unwrap(),
                        lost[(n + TORN - RECORDS) as usize]
                    );
                }
            }
            store.flush().unwrap();
            let all = [kept.clone(), lost.clone()].concat();
            let all = if fault == Fault::Zeroed { &all } else { &kept };
            holds(&store, all, &[], &case);
            drop(store);
            let synced = len(&pack);
            holds(
                &open(dir.path(), Basis::Boundary(synced), false).unwrap(),
                all,
                &[],
                &case,
            );
        }
    }
}

#[test]
fn the_same_record_before_the_boundary_is_damage_and_the_pack_is_left_as_it_is() {
    for fault in FAULTS {
        for locked in [true, false] {
            let dir = alt_os::temp::tempdir().unwrap();
            let pack = dir.path().join("pack-00000001.altpack");
            let (kept, lost, _) = torn(dir.path(), fault);
            let before = fs::read(&pack).unwrap();
            let case = format!("{fault:?}, locked {locked}");
            match open(dir.path(), Basis::Boundary(len(&pack)), locked) {
                // records known synced are not read when the store opens:
                // reading the chunk and the full check report them
                Ok(store) => {
                    assert_ne!(fault, Fault::Zeros, "{case}");
                    for id in &kept {
                        assert_eq!(ChunkId::of(&store.get(*id).unwrap()), *id, "{case}");
                    }
                    for id in &lost {
                        let err = store.get(*id).unwrap_err();
                        assert!(matches!(err, StoreError::Corrupt { .. }), "{case}: {err:?}");
                    }
                    assert!(store.check().is_err(), "{case}");
                }
                // the zeros end short of a whole record
                Err(e) => {
                    assert_eq!(fault, Fault::Zeros, "{case}: {e:?}");
                    assert!(
                        matches!(e, StoreError::Format("active pack damaged")),
                        "{case}: {e:?}"
                    );
                }
            }
            assert_eq!(fs::read(&pack).unwrap(), before, "{case}");
        }
    }
}

#[test]
fn with_no_boundary_the_records_at_the_end_that_do_not_hold_never_finished() {
    for fault in FAULTS {
        for locked in [true, false] {
            let dir = alt_os::temp::tempdir().unwrap();
            let pack = dir.path().join("pack-00000001.altpack");
            let (kept, lost, at) = torn(dir.path(), fault);
            let before = fs::read(&pack).unwrap();
            let case = format!("{fault:?}, locked {locked}");
            let store = open(dir.path(), Basis::LastOnly, locked).unwrap();
            holds(&store, &kept, &lost, &case);
            drop(store);
            match locked {
                true => assert_eq!(len(&pack), at, "{case}"),
                false => assert_eq!(fs::read(&pack).unwrap(), before, "{case}"),
            }
        }
    }
}

#[test]
fn with_no_boundary_a_record_that_does_not_hold_before_sound_ones_is_not_cut() {
    let dir = alt_os::temp::tempdir().unwrap();
    let pack = dir.path().join("pack-00000001.altpack");
    let (_, lost, at) = torn(dir.path(), Fault::Half);
    let mut store = ChunkStore::open(dir.path()).unwrap();
    store.put(&chunk(RECORDS)).unwrap();
    store.flush().unwrap();
    drop(store);
    assert!(len(&pack) > at);
    let before = fs::read(&pack).unwrap();
    let store = open(dir.path(), Basis::LastOnly, true).unwrap();
    assert!(matches!(
        store.get(lost[0]),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(store.check().is_err());
    assert_eq!(fs::read(&pack).unwrap(), before);
}

#[test]
fn with_what_was_recorded_gone_nothing_is_an_unfinished_append() {
    for fault in FAULTS {
        let dir = alt_os::temp::tempdir().unwrap();
        let pack = dir.path().join("pack-00000001.altpack");
        let (_, lost, _) = torn(dir.path(), fault);
        let before = fs::read(&pack).unwrap();
        match open(dir.path(), Basis::Unknown, true) {
            Ok(store) => {
                assert_ne!(fault, Fault::Zeros);
                for id in lost {
                    let err = store.get(id).unwrap_err();
                    assert!(
                        matches!(err, StoreError::Corrupt { .. }),
                        "{fault:?}: {err:?}"
                    );
                }
                assert!(store.check().is_err(), "{fault:?}");
            }
            Err(e) => {
                assert_eq!(fault, Fault::Zeros, "{e:?}");
                assert!(
                    matches!(e, StoreError::Format("active pack damaged")),
                    "{e:?}"
                );
            }
        }
        assert_eq!(fs::read(&pack).unwrap(), before, "{fault:?}");
    }
}
