//! Callers that share one ref store are answered by the tickets they
//! take for what they appended: each is told of a cut of its own ops,
//! and of no other.

use std::path::Path;

use alt_oplog::{OpLog, OpLogError, OpLogSink};
use alt_refs::{Answer, RefStore, RefTarget, Ticket};

use super::owed_sync::{commit, cut_unsynced, goes_on};
use super::v2::{boot, new_store, oid};

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

/// Whether a store opened now holds a ref that points at object `n`.
fn in_the_log(dir: &Path, n: u8) -> bool {
    let store = RefStore::open_read_only(dir).unwrap();
    store
        .iter()
        .any(|(_, target)| *target == RefTarget::Oid(oid(n)))
}

/// What the holder of `ticket` is answered by the sink of `store`, the
/// store asked again where the sink has nothing to sync its ops up to.
fn answered(store: &mut RefStore, sink: &OpLogSink, ticket: &Ticket) -> Result<(), OpLogError> {
    loop {
        match sink.fsync_for(ticket)? {
            Answer::Synced => return Ok(()),
            Answer::Owed => store.sync_for(ticket).unwrap(),
        }
    }
}

#[test]
fn of_the_commits_one_fsync_was_to_cover_each_whose_transaction_was_cut_is_told_and_no_other() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut store = new_store(dir.path());
    store.set_defer_durability(true);
    let sink = store.sync_handle().unwrap();
    let mut tickets = Vec::new();
    for n in 1..=4 {
        commit(&mut store, &format!("refs/heads/b{n}"), n).unwrap();
        tickets.push(store.ticket());
    }
    // the last two are cut before the fsync all four wait for
    let log = dir.path().join("oplog");
    let _ = std::fs::remove_file(log.join("tail"));
    let mut other = OpLog::open(&log).unwrap();
    let held = |op: &alt_oplog::Op| Ok::<_, OpLogError>(op.frame.unwrap().seq <= 2);
    assert_eq!(other.settle(held).unwrap().unwrap().ops, 2);
    drop(other);
    let reached = sink.reached().unwrap();
    sink.fsync_to(reached).unwrap();
    for (n, ticket) in tickets.iter().enumerate() {
        let told = match n < 2 {
            true => answered(&mut store, &sink, ticket),
            false => sink.answer(ticket).map(drop),
        };
        assert_eq!(told.is_ok(), n < 2, "commit {}: {told:?}", n + 1);
        assert_eq!(in_the_log(dir.path(), n as u8 + 1), n < 2);
    }
    assert!(store.confirmed());
    assert_eq!(store.len(), 2);
}

/// One sequence of commits by callers that hold a ticket each, cuts by
/// another handle, the store going on past them, and callers asking
/// the sink in any order. Each is told that its commit is synced, and
/// its transaction is in the log, or told of a cut, and it is not.
/// Returns how many came to each end.
fn sequence(seed: u64) -> (u64, u64) {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut store = new_store(dir.path());
    store.set_defer_durability(true);
    let sink = store.sync_handle().unwrap();
    let mut rng = Rng(seed);
    let (mut waiting, mut next, mut ends) = (Vec::<(u8, Ticket)>::new(), 0u8, (0, 0));
    // how many ops the log held when a sync was last recorded
    let mut synced = 0;
    for step in 0..30 {
        let case = format!("seed {seed}, step {step}");
        match rng.below(6) {
            0 | 1 => {
                next += 1;
                let name = format!("refs/heads/b{next}");
                let committed = commit(&mut store, &name, next);
                assert!(committed.is_ok(), "{case}: {committed:?}");
                waiting.push((next, store.ticket()));
            }
            2 => drop(cut_unsynced(dir.path(), synced)),
            3 => goes_on(&mut store, rng.below(2) == 0),
            _ if waiting.is_empty() => {}
            _ => {
                let (n, ticket) = waiting.remove(rng.below(waiting.len() as u64) as usize);
                let told = answered(&mut store, &sink, &ticket);
                let held = in_the_log(dir.path(), n);
                match told {
                    Ok(()) => {
                        assert!(held, "{case}: commit {n} is told as synced");
                        let log = OpLog::open_read_only(&dir.path().join("oplog"), None);
                        synced = log.unwrap().len() as u64;
                        ends.0 += 1;
                    }
                    Err(OpLogError::CutBack) => {
                        assert!(!held, "{case}: commit {n} is told as cut");
                        ends.1 += 1;
                    }
                    Err(other) => panic!("{case}: {other:?}"),
                }
            }
        }
    }
    ends
}

#[test]
fn whatever_a_store_does_past_cuts_each_caller_is_told_what_became_of_its_own_commit() {
    let (mut synced, mut told) = (0, 0);
    for seed in 0..300 {
        let ends = sequence(seed);
        (synced, told) = (synced + ends.0, told + ends.1);
    }
    eprintln!("{synced} commits synced, {told} told of as cut");
    assert!(synced > 100 && told > 100);
}
