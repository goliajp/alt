//! Callers that share one ref store are answered by the tickets the
//! store wrote for them on: each is told of a cut of its own ops, and
//! of no other.

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

/// How many ops of the log in `dir` lie within where it is recorded as
/// synced.
fn recorded(dir: &Path) -> u64 {
    let log = dir.join("oplog");
    let bound = alt_oplog::bounded_in(&log).unwrap_or(0);
    let log = OpLog::open_read_only(&log, None).unwrap();
    let within = |op: &&alt_oplog::Op| op.frame.is_some_and(|frame| frame.end <= bound);
    log.ops().unwrap().iter().filter(within).count() as u64
}

/// Has `store` commit a ref that points at object `n`, for a caller of
/// its own; the ticket of that caller.
fn committed(store: &mut RefStore, n: u8) -> Ticket {
    let name = format!("refs/heads/b{n}");
    let committed = store.ticketed(|store| commit(store, &name, n));
    let ((), ticket) = committed.unwrap();
    assert!(!ticket.is_empty());
    ticket
}

/// What the holder of `ticket` is answered by the sink of `store`.
/// Where the sink has nothing to sync its ops up to, the store is
/// asked, and what it answers is what became of them.
fn answered(store: &mut RefStore, sink: &OpLogSink, ticket: &Ticket) -> Result<(), OpLogError> {
    if sink.fsync_for(ticket)? == Answer::Synced {
        return Ok(());
    }
    let told = match store.sync_for(ticket) {
        Ok(()) => Ok(()),
        Err(alt_refs::RefError::OpLog(told)) => Err(told),
        Err(other) => panic!("{other:?}"),
    };
    // and the sink says the same from then on
    assert_eq!(sink.answer(ticket).is_ok(), told.is_ok());
    told
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
        tickets.push(committed(&mut store, n));
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

/// The sink of a store on `dir` is asked without a ticket, for the
/// commits `left` that were made for the caller the store has without
/// one: told that they are synced (2), they are in the log; or told of
/// a cut (3), of some commit.
fn sink_asked(dir: &Path, sink: &OpLogSink, left: &[u8], case: &str) -> usize {
    match sink.fsync() {
        Ok(()) => {
            for n in left {
                assert!(in_the_log(dir, *n), "{case}: commit {n} is not");
            }
            2
        }
        Err(OpLogError::CutBack) => {
            assert!(!left.is_empty(), "{case}: told of a cut of nothing");
            3
        }
        Err(other) => panic!("{case}: {other:?}"),
    }
}

/// One sequence of commits by callers that hold a ticket each and by
/// the caller the store has without one, cuts by another handle, the
/// store going on past them, and callers asking the sink in any order.
/// Each with a ticket is told that its commit is synced, and its
/// transaction is in the log, or told of a cut, and it is not. The one
/// without is told that the commits made for it since it last asked
/// are synced, and they are in the log, or told of a cut. Returns how
/// many came to each end: tickets synced and cut, then commits without
/// a ticket synced, and times their caller was told of a cut.
fn sequence(seed: u64) -> [u64; 4] {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut store = new_store(dir.path());
    store.set_defer_durability(true);
    let sink = store.sync_handle().unwrap();
    let mut rng = Rng(seed);
    let (mut waiting, mut next, mut ends) = (Vec::<(u8, Ticket)>::new(), 0u8, [0; 4]);
    // the commits made without a ticket that the sink was not asked for
    let mut unticketed = Vec::<u8>::new();
    // how many ops the log was ever recorded as synced over
    let mut synced = 0;
    for step in 0..30 {
        let case = format!("seed {seed}, step {step}");
        // an fsync of the sink records a sync whatever it answers after
        synced = synced.max(recorded(dir.path()));
        match rng.below(8) {
            0 | 1 => {
                next += 1;
                waiting.push((next, committed(&mut store, next)));
            }
            2 => drop(cut_unsynced(dir.path(), synced)),
            3 => goes_on(&mut store, rng.below(2) == 0),
            4 => {
                next += 1;
                let committed = commit(&mut store, &format!("refs/heads/b{next}"), next);
                assert!(committed.is_ok(), "{case}: {committed:?}");
                unticketed.push(next);
            }
            5 => {
                let left = std::mem::take(&mut unticketed);
                let end = sink_asked(dir.path(), &sink, &left, &case);
                ends[end] += if end == 2 { left.len() as u64 } else { 1 };
            }
            _ if waiting.is_empty() => {}
            _ => {
                let (n, ticket) = waiting.remove(rng.below(waiting.len() as u64) as usize);
                let told = answered(&mut store, &sink, &ticket);
                let held = in_the_log(dir.path(), n);
                match told {
                    Ok(()) => {
                        assert!(held, "{case}: commit {n} is told as synced");
                        ends[0] += 1;
                    }
                    Err(OpLogError::CutBack) => {
                        assert!(!held, "{case}: commit {n} is told as cut");
                        ends[1] += 1;
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
    let mut ends = [0; 4];
    for seed in 0..300 {
        let of_one = sequence(seed);
        ends = std::array::from_fn(|end| ends[end] + of_one[end]);
    }
    eprintln!(
        "{} commits on tickets synced, {} told of as cut; {} without a ticket synced, \
         their caller told of a cut {} times",
        ends[0], ends[1], ends[2], ends[3]
    );
    assert!(ends.iter().all(|times| *times > 100), "{ends:?}");
}
