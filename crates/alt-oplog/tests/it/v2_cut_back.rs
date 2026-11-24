//! A handle on a version 2 log is told that its ops were cut when they
//! were, and only then.

use std::path::Path;
use std::sync::mpsc::channel;

use alt_oplog::{Meta, Op, OpLog, OpLogError};

use super::v2_bounds::stage;
use super::v2_log::{boot, file_len, grow, new_log};
use super::v2_named::{Given, collecting};

pub fn but_left(op: &Op) -> Result<bool, OpLogError> {
    Ok(op.payload != b"left")
}

/// A handle of boot 2 on a log in `dir` whose two synced ops boot 1 left
/// with a ref transaction `left` after them. A writer that looks at
/// nothing appended forty ops over blocks since, among them one that has
/// to be known to append after it when `required`: the handle begins at
/// a checkpoint of its boot, past `left`, and syncs what a sync has to
/// see to.
pub fn held_past_left(dir: &Path, deferred: bool, required: bool) -> (OpLog, Given) {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    let linked = Meta {
        linked: true,
        ..Meta::default()
    };
    log.append_with("a", 3, b"left", &linked).unwrap();
    drop(log);
    boot(2);
    let mut staging = OpLog::open(dir).unwrap();
    stage(&mut staging, 20);
    if required {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        let must = Meta {
            required: true,
            ..Meta::default()
        };
        staging.append_with("a", 4, b"known", &must).unwrap();
    }
    stage(&mut staging, 20);
    drop(staging);
    let mut held = OpLog::open(dir).unwrap();
    assert!(held.recent().iter().all(|op| op.payload != b"left"));
    let given = collecting(&mut held);
    held.set_defer_durability(deferred);
    (held, given)
}

/// What a handle is told by each of the things it reads without a lock
/// on the log: its sync, its full check, the ops before the ones it
/// holds.
pub fn told(held: &mut OpLog) -> String {
    let synced = held.sync().err();
    format!("{:?}", (synced, held.verify().err(), held.ops().err()))
}

pub const CUT_BACK: &str = "(Some(CutBack), Some(CutBack), Some(CutBack))";

#[test]
fn the_sink_of_a_handle_that_holds_no_frame_syncs_and_is_told_of_no_cut() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    log.set_defer_durability(true);
    let sink = log.sync_handle().unwrap();
    log.sync().unwrap();
    sink.fsync().unwrap();
    log.append("a", 1, b"x").unwrap();
    log.sync().unwrap();
    sink.fsync().unwrap();
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(log.appended_len()));
}

#[test]
fn a_handle_whose_ops_another_cut_is_told_so_when_it_syncs_and_not_that_the_log_is_damaged() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut held, _given) = held_past_left(dir.path(), deferred, false);
        let mut cutter = OpLog::open(dir.path()).unwrap();
        assert_eq!(cutter.settle(but_left).unwrap().unwrap().ops, 41);
        // the ref transaction a sync has to see to is no longer where
        // the frames this handle holds say it is
        let synced = held.sync();
        assert!(
            matches!(synced, Err(OpLogError::CutBack)),
            "deferred: {deferred}, {synced:?}"
        );
        held.reopen().unwrap();
        assert_eq!(held.len(), 2);
        held.sync().unwrap();
    }
}

#[test]
fn a_cut_that_died_with_its_checkpoint_laid_over_an_op_tells_a_handle_past_it_that_it_was_cut() {
    // the checkpoint is written, and synced: the file is as long as it was
    for (step, deferred) in [(2, false), (2, true), (3, false), (3, true)] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut held, _given) = held_past_left(dir.path(), deferred, false);
        let len = file_len(dir.path());
        let mut cutter = OpLog::open(dir.path()).unwrap();
        alt_oplog::v2::cut_dies_after(Some(step));
        let cut = cutter.settle(but_left);
        alt_oplog::v2::cut_dies_after(None);
        assert!(cut.is_err(), "the cut died");
        drop(cutter);
        assert_eq!(file_len(dir.path()), len);
        assert_eq!(told(&mut held), CUT_BACK, "step {step}, {deferred}");
        held.reopen().unwrap();
        assert_eq!(held.len(), 2);
        held.sync().unwrap();
    }
}

#[test]
fn a_handle_past_an_op_another_thread_is_cutting_the_log_at_is_told_that_it_was_cut() {
    for (step, deferred) in [(2, false), (2, true), (3, false), (3, true)] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut held, _given) = held_past_left(dir.path(), deferred, false);
        let len = file_len(dir.path());
        let (reached, is_reached) = channel();
        let (go, goes) = channel();
        let path = dir.path().to_owned();
        let cutting = std::thread::spawn(move || {
            boot(2);
            let mut cutter = OpLog::open(&path).unwrap();
            alt_oplog::v2::cut_waits_after(step, reached, goes);
            cutter.settle(but_left).unwrap().unwrap().ops
        });
        // the cutter holds its locks, with its checkpoint over `left`
        is_reached.recv().unwrap();
        assert_eq!(file_len(dir.path()), len);
        let told = told(&mut held);
        go.send(()).unwrap();
        assert_eq!(cutting.join().unwrap(), 41);
        assert_eq!(told, CUT_BACK, "step {step}, {deferred}");
        held.reopen().unwrap();
        assert_eq!(held.len(), 2);
        held.sync().unwrap();
    }
}

#[test]
fn a_handle_that_holds_a_frame_to_be_known_where_the_log_was_cut_is_told_that_it_was_cut() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut held, _given) = held_past_left(dir.path(), deferred, true);
        let mut cutter = OpLog::open(dir.path()).unwrap();
        assert_eq!(cutter.settle(but_left).unwrap().unwrap().ops, 42);
        drop(cutter);
        // the frame that has to be known lies past the end of the file
        let synced = held.sync();
        assert!(
            matches!(synced, Err(OpLogError::CutBack)),
            "deferred: {deferred}, {synced:?}"
        );
        held.reopen().unwrap();
        assert_eq!(held.len(), 2);
        held.sync().unwrap();
    }
}
