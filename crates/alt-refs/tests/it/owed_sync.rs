//! A ref store goes on past a cut of its log without telling its caller,
//! and no sync says for that of a transaction that was cut that it is
//! synced: the sync that was to cover it tells of the cut, once.

use std::path::Path;

use alt_oplog::{OpLog, OpLogError, OpLogSink};
use alt_refs::{RefChange, RefError, RefStore, RefTarget};

use super::v2::{boot, held, new_store, oid, point, store_with_unsynced};

pub fn commit(store: &mut RefStore, name: &str, at: u8) -> Result<(), RefError> {
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
pub fn cut_unsynced(dir: &Path, synced: u64) -> usize {
    let log = dir.join("oplog");
    let _ = std::fs::remove_file(log.join("tail"));
    let mut other = OpLog::open(&log).unwrap();
    let held = |op: &alt_oplog::Op| Ok::<_, OpLogError>(op.frame.unwrap().seq <= synced);
    other.settle(held).unwrap().map_or(0, |cut| cut.ops)
}

pub fn cut_back<T: std::fmt::Debug>(told: Result<T, RefError>) -> bool {
    match told {
        Err(RefError::OpLog(OpLogError::CutBack)) => true,
        Err(RefError::Recorded { cause, .. }) => {
            matches!(*cause, RefError::OpLog(OpLogError::CutBack))
        }
        _ => false,
    }
}

pub fn goes_on(store: &mut RefStore, follows: bool) {
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
fn a_commit_after_a_cut_is_synced_by_the_sink_that_tells_of_the_transactions_cut_before_it() {
    for follows in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut store, sink) = left_to_a_sink(dir.path());
        goes_on(&mut store, follows);
        // its transaction is in the log: the commit is told of no cut
        commit(&mut store, "refs/heads/main", 3).unwrap();
        // whoever waits for the sink is, once, and the commit is synced
        let told = sink.fsync();
        assert!(
            matches!(told, Err(OpLogError::CutBack)),
            "follows: {follows}, {told:?}"
        );
        assert!(store.confirmed(), "follows: {follows}");
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

/// A store held open over a cut another boot's writer made of the three
/// ops it left unsynced, a transaction built on them refused: its refs
/// are those of the log as it is.
fn held_open_over_a_cut(dir: &Path, deferred: bool) -> RefStore {
    let mut held_open = store_with_unsynced(dir);
    held_open.set_defer_durability(deferred);
    boot(2);
    let mut writer = RefStore::open(dir).unwrap();
    assert!(writer.settle(held).unwrap().is_some());
    drop(writer);
    let stale = RefChange {
        name: "refs/heads/main".to_owned(),
        old: Some(RefTarget::Oid(oid(2))),
        new: Some(RefTarget::Oid(oid(7))),
    };
    // it reads the log again by itself and finds the conflict
    let refused = held_open.commit("a", 7, &[stale]).unwrap_err();
    assert!(matches!(refused, RefError::Conflict { .. }), "{refused:?}");
    let main = held_open.get("refs/heads/main");
    assert_eq!(main, Some(&RefTarget::Oid(oid(1))));
    assert_eq!(held_open.get("refs/heads/dev"), None);
    held_open
}

#[test]
fn a_store_held_open_follows_a_cut_made_by_another_and_the_sync_that_was_to_cover_the_ops_tells() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut held_open = held_open_over_a_cut(dir.path(), deferred);
        // once: by the sink the ops were left to, or, with the store
        // syncing by itself since, by its next sync
        match deferred {
            true => {
                let sink = held_open.sync_handle().unwrap();
                let told = sink.fsync();
                assert!(matches!(told, Err(OpLogError::CutBack)), "{told:?}");
                point(&mut held_open, "refs/heads/main", 8);
                sink.fsync().unwrap();
            }
            false => {
                assert!(cut_back(commit(&mut held_open, "refs/heads/main", 8)));
                // the transaction is in the log, and the next sync covers it
                held_open.bound().unwrap();
            }
        }
        let main = held_open.get("refs/heads/main");
        assert_eq!(main, Some(&RefTarget::Oid(oid(8))));
        drop(held_open);
        let store = RefStore::open(dir.path()).unwrap();
        assert_eq!(store.get("refs/heads/main"), Some(&RefTarget::Oid(oid(8))));
        store.check().unwrap();
    }
}

#[test]
fn a_store_whose_sync_tells_of_a_cut_follows_the_log_that_read_itself_anew_for_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut store = new_store(dir.path());
    commit(&mut store, "refs/heads/main", 1).unwrap();
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        commit(&mut store, "refs/heads/dev", 2).unwrap();
    }
    assert_eq!(cut_unsynced(dir.path(), 1), 1);
    // the store holds the transaction that is gone until the sync
    assert!(cut_back(store.bound()));
    assert_eq!((store.len(), store.get("refs/heads/dev")), (1, None));
    commit(&mut store, "refs/heads/next", 3).unwrap();
    assert_eq!(store.len(), 2);
}

#[test]
fn an_op_recorded_without_a_sync_that_was_cut_is_told_of_by_the_sync_of_the_op_after_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut store = new_store(dir.path());
    commit(&mut store, "refs/heads/main", 1).unwrap();
    for round in 0..2 {
        // a sync of the store tells of a cut, and is over with that
        {
            let _unsynced = alt_os::durability::leave_unsynced(true);
            commit(&mut store, "refs/heads/dev", 2).unwrap();
        }
        assert_eq!(cut_unsynced(dir.path(), 1), 1);
        store.refresh().unwrap();
        assert!(cut_back(store.bound()), "round {round}");
        // the next op that is recorded without a sync is cut, and the
        // sync of the op after it tells so
        store
            .record_op_unsynced("a", 3, b"\x40 a step begun")
            .unwrap();
        assert_eq!(cut_unsynced(dir.path(), 1), 1);
        store.refresh().unwrap();
        let recorded = store.record_op("a", 4, b"\x40 a step done");
        assert!(cut_back(recorded), "round {round}");
        let log = OpLog::open(&dir.path().join("oplog")).unwrap();
        assert_eq!(log.len(), 2, "round {round}");
        assert_eq!(cut_unsynced(dir.path(), 1), 1);
        store.refresh().unwrap();
        assert!(cut_back(store.bound()), "round {round}");
        store.bound().unwrap();
    }
}
