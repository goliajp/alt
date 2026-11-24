//! A ref store goes on past a cut of its log without telling its caller,
//! and no sync says for that of a transaction that was cut that it is
//! synced: the sync that was to cover it tells of the cut, once.

use std::path::Path;

use alt_oplog::{OpLog, OpLogError, OpLogSink};
use alt_refs::{RefChange, RefError, RefStore, RefTarget};

use super::v2::{boot, new_store, oid};

fn commit(store: &mut RefStore, name: &str, at: u8) -> Result<(), RefError> {
    let change = RefChange {
        old: store.get(name).cloned(),
        new: Some(RefTarget::Oid(oid(at))),
        name: name.to_owned(),
    };
    store.commit("a", at as u64, &[change]).map(drop)
}

/// Has another handle, which finds no note beside the log, cut every op
/// after the first `synced` ops of the log, which are synced and what
/// they name with them; how many.
fn cut_unsynced(dir: &Path, synced: u64) -> usize {
    let log = dir.join("oplog");
    let _ = std::fs::remove_file(log.join("tail"));
    let mut other = OpLog::open(&log).unwrap();
    let held = |op: &alt_oplog::Op| Ok::<_, OpLogError>(op.frame.unwrap().seq <= synced);
    other.settle(held).unwrap().map_or(0, |cut| cut.ops)
}

fn cut_back<T: std::fmt::Debug>(told: Result<T, RefError>) -> bool {
    match told {
        Err(RefError::OpLog(OpLogError::CutBack)) => true,
        Err(RefError::Recorded { cause, .. }) => {
            matches!(*cause, RefError::OpLog(OpLogError::CutBack))
        }
        _ => false,
    }
}

fn goes_on(store: &mut RefStore, follows: bool) {
    match follows {
        true => store.follow().unwrap(),
        false => store.refresh().unwrap(),
    }
}

/// A store that leaves its syncs to a sink, with two transactions left
/// to it that another handle then cut.
fn left_to_a_sink(dir: &Path) -> (RefStore, OpLogSink) {
    boot(1);
    let mut store = new_store(dir);
    store.set_defer_durability(true);
    let sink = store.sync_handle().unwrap();
    commit(&mut store, "refs/heads/main", 1).unwrap();
    commit(&mut store, "refs/heads/dev", 2).unwrap();
    assert_eq!(cut_unsynced(dir, 0), 2);
    (store, sink)
}

#[test]
fn a_store_that_goes_on_past_a_cut_leaves_its_sink_to_tell_of_the_transactions_cut() {
    for follows in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut store, sink) = left_to_a_sink(dir.path());
        goes_on(&mut store, follows);
        assert_eq!(store.len(), 0);
        let told = sink.fsync();
        assert!(
            matches!(told, Err(OpLogError::CutBack)),
            "follows: {follows}, {told:?}"
        );
        // once; a transaction committed since is synced as ever
        sink.fsync().unwrap();
        commit(&mut store, "refs/heads/main", 3).unwrap();
        sink.fsync().unwrap();
        assert!(store.confirmed());
        drop(store);
        assert_eq!(RefStore::open(dir.path()).unwrap().len(), 1);
    }
}

#[test]
fn a_commit_after_a_cut_is_not_left_to_the_sink_over_the_transactions_it_still_has_to_tell_of() {
    for follows in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut store, sink) = left_to_a_sink(dir.path());
        goes_on(&mut store, follows);
        // the commit is told, and so is whoever waits for the sink
        let committed = commit(&mut store, "refs/heads/main", 3);
        assert!(cut_back(committed), "follows: {follows}");
        let told = sink.fsync();
        assert!(
            matches!(told, Err(OpLogError::CutBack)),
            "follows: {follows}, {told:?}"
        );
        commit(&mut store, "refs/heads/dev", 4).unwrap();
        sink.fsync().unwrap();
        assert!(store.confirmed());
    }
}

#[test]
fn a_store_that_syncs_itself_tells_with_its_next_sync_of_a_transaction_cut_before_it_was_synced() {
    for follows in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut store = new_store(dir.path());
        commit(&mut store, "refs/heads/main", 1).unwrap();
        {
            let _unsynced = alt_os::durability::leave_unsynced(true);
            commit(&mut store, "refs/heads/dev", 2).unwrap();
        }
        assert_eq!(cut_unsynced(dir.path(), 1), 1);
        goes_on(&mut store, follows);
        assert_eq!(store.len(), 1);
        assert!(cut_back(store.bound()), "follows: {follows}");
        store.bound().unwrap();
        assert!(store.confirmed());
    }
}

/// Numbers that follow from a seed.
struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (mixed ^ (mixed >> 31)) % bound
    }
}

/// The refs a store opened now holds, by the number each points at.
fn in_the_log(dir: &Path) -> Vec<u8> {
    let store = RefStore::open_read_only(dir).unwrap();
    let pointed = |n: u8| {
        store
            .iter()
            .any(|(_, target)| *target == RefTarget::Oid(oid(n)))
    };
    (1..=40).filter(|n| pointed(*n)).collect()
}

/// One sequence of commits left to a sink, cuts by another handle, the
/// store going on past them, and fsyncs of the sink. Each commit left to
/// the sink comes to one of two ends: the sink syncs it, and its
/// transaction is in the log within what is synced, or it is told of as
/// cut, once, by the sink or by the commit that would have been laid
/// over it. Returns how many came to each.
fn sequence(seed: u64) -> (u64, u64) {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut store = new_store(dir.path());
    store.set_defer_durability(true);
    let sink = store.sync_handle().unwrap();
    let mut rng = Rng(seed);
    // the commits the sink was left with and has given no answer for
    let (mut left, mut next, mut ends) = (Vec::new(), 0u8, (0, 0));
    // how many ops the log held when the sink last synced it, and
    // whether ops the store holds were cut since it last read the log
    let (mut synced, mut stale) = (0, false);
    for step in 0..30 {
        let case = format!("seed {seed}, step {step}");
        match rng.below(6) {
            0 | 1 => {
                next += 1;
                let name = format!("refs/heads/b{next}");
                match commit(&mut store, &name, next) {
                    Ok(()) => left.push(next),
                    // it holds ops that are gone, or its sink still owes
                    // an answer for some
                    told => assert!(cut_back(told) && (stale || !left.is_empty()), "{case}"),
                }
            }
            2 => stale |= cut_unsynced(dir.path(), synced) > 0,
            3 => {
                goes_on(&mut store, rng.below(2) == 0);
                stale = false;
            }
            _ => match sink.fsync() {
                Ok(()) => {
                    let held = in_the_log(dir.path());
                    for n in left.drain(..) {
                        assert!(held.contains(&n), "{case}: commit {n} is told as synced");
                        ends.0 += 1;
                    }
                    let log = OpLog::open_read_only(&dir.path().join("oplog"), None);
                    synced = log.unwrap().len() as u64;
                }
                Err(OpLogError::CutBack) => {
                    let held = in_the_log(dir.path());
                    assert!(!left.is_empty(), "{case}: told of a cut of nothing");
                    assert!(
                        left.iter().any(|n| !held.contains(n)),
                        "{case}: told of a cut, and the log holds what was left"
                    );
                    ends.1 += left.drain(..).count() as u64;
                }
                Err(other) => panic!("{case}: {other:?}"),
            },
        }
    }
    ends
}

#[test]
fn whatever_a_store_does_past_cuts_each_commit_left_to_its_sink_is_synced_or_told_of_once() {
    let (mut synced, mut told) = (0, 0);
    for seed in 0..300 {
        let ends = sequence(seed);
        (synced, told) = (synced + ends.0, told + ends.1);
    }
    eprintln!("{synced} commits synced by the sink, {told} told of as cut");
    assert!(synced > 100 && told > 100);
}
