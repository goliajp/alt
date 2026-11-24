//! A handle on a version 2 log that holds ops which were cut is told so
//! by whatever it reads the log with, and by nothing else. A sync tells
//! of a cut to whoever holds frames that were cut, and reads the log
//! again for a handle that holds none of its own.

use std::fs;
use std::path::Path;
use std::sync::mpsc::channel;

use alt_oplog::{Meta, Op, OpLog, OpLogError};

use super::v2_bounds::stage;
use super::v2_image::linked;
use super::v2_log::{boot, file_len, grow, new_log};
use super::v2_named::{Given, collecting};
use super::v2_reread::cut_from;
use super::v2_unseen::seeing;

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
/// on the log: its full check, the ops before the ones it holds.
pub fn told(held: &OpLog) -> String {
    format!("{:?}", (held.verify().err(), held.ops().err()))
}

pub const CUT_BACK: &str = "(Some(CutBack), Some(CutBack))";

/// A handle that wrote none of the ops which were cut syncs: it reads
/// the log again by itself, and holds the two ops left.
pub fn syncs_what_is_left(held: &mut OpLog, case: &str) {
    let reads = held.reads();
    held.sync()
        .unwrap_or_else(|told| panic!("{case}: {told:?}"));
    if let Ok(sink) = held.sync_handle() {
        sink.fsync()
            .unwrap_or_else(|told| panic!("{case}: {told:?}"));
    }
    assert_eq!((held.len(), held.reads()), (2, reads + 1), "{case}");
}

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
fn a_handle_past_ops_another_cut_syncs_what_is_left_and_is_not_told_that_the_log_is_damaged() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut held, _given) = held_past_left(dir.path(), deferred, false);
        let mut cutter = OpLog::open(dir.path()).unwrap();
        assert_eq!(cutter.settle(but_left).unwrap().unwrap().ops, 41);
        // the ref transaction a sync has to see to is no longer where
        // the frames this handle holds say it is
        syncs_what_is_left(&mut held, &format!("deferred: {deferred}"));
        assert!(held.confirmed());
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
        assert_eq!(told(&held), CUT_BACK, "step {step}, {deferred}");
        syncs_what_is_left(&mut held, &format!("step {step}, {deferred}"));
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
        let told = told(&held);
        go.send(()).unwrap();
        assert_eq!(cutting.join().unwrap(), 41);
        assert_eq!(told, CUT_BACK, "step {step}, {deferred}");
        syncs_what_is_left(&mut held, &format!("step {step}, {deferred}"));
    }
}

#[test]
fn a_handle_that_holds_a_frame_to_be_known_where_the_log_was_cut_syncs_what_is_left() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let (mut held, _given) = held_past_left(dir.path(), deferred, true);
        let mut cutter = OpLog::open(dir.path()).unwrap();
        assert_eq!(cutter.settle(but_left).unwrap().unwrap().ops, 42);
        drop(cutter);
        // the frame that has to be known lies past the end of the file
        syncs_what_is_left(&mut held, &format!("deferred: {deferred}"));
    }
}

#[test]
fn a_sink_tells_once_of_the_cut_of_what_it_was_left_with_though_its_handle_was_told_and_read_anew()
{
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut held = new_log(dir.path());
    grow(&mut held, "a", 2);
    held.set_defer_durability(true);
    let sink = held.sync_handle().unwrap();
    // its sink is left to sync an op of the handle
    held.append("a", 8, b"mine").unwrap();
    held.sync().unwrap();
    assert_eq!(cut_from(dir.path(), b"mine"), 1);
    let appended = held.append("a", 9, b"more");
    assert!(matches!(appended, Err(OpLogError::CutBack)), "{appended:?}");
    held.reopen().unwrap();
    assert_eq!(held.len(), 2);
    // whoever waits for that sync was not told by the append
    let told = sink.fsync();
    assert!(matches!(told, Err(OpLogError::CutBack)), "{told:?}");
    sink.fsync().unwrap();
    held.append("a", 9, b"more").unwrap();
    held.sync().unwrap();
    sink.fsync().unwrap();
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(held.appended_len()));
}

#[test]
fn a_sync_that_reads_an_op_from_before_a_cut_whose_checkpoint_a_cut_lays_its_own_over_tells_of_no_damage()
 {
    let dir = alt_os::temp::tempdir().unwrap();
    let path = dir.path().join("log");
    boot(1);
    alt_oplog::v2::create_with(dir.path(), 10).unwrap();
    let mut log = seeing(OpLog::open(dir.path()).unwrap());
    grow(&mut log, "a", 2);
    let last = {
        // a ref transaction, ops over blocks after it, and two short ones
        let _unsynced = alt_os::durability::leave_unsynced(true);
        log.append_with("a", 3, b"tx", &linked()).unwrap();
        for n in 0..10 {
            log.append("pad", n, &[n as u8; 150]).unwrap();
        }
        log.append("a", 5, b"p").unwrap();
        log.append("a", 6, b"last").unwrap();
        log.anchor().unwrap().start as usize
    };
    drop(log);
    // the last op is cut: the checkpoint of that cut tells under which
    // generation the ops before it read. A handle opened then begins
    // where the log ends, past the ref transaction
    boot(2);
    let but =
        |gone: &'static [u8]| move |op: &alt_oplog::Op| Ok::<_, OpLogError>(op.payload != gone);
    let mut first = OpLog::open(dir.path()).unwrap();
    assert_eq!(first.settle(but(b"last")).unwrap().unwrap().ops, 1);
    drop(first);
    let mut held = seeing(OpLog::open(dir.path()).unwrap());
    assert!(!held.unsettled() && held.recent().len() < held.len());
    held.set_defer_durability(true);
    let sink = held.sync_handle().unwrap();
    // a writer that finds no note cuts the op before, and dies with its
    // checkpoint laid: it lies over the one of the cut before
    fs::remove_file(dir.path().join("tail")).unwrap();
    let before = fs::read(&path).unwrap();
    let mut cutter = OpLog::open(dir.path()).unwrap();
    alt_oplog::v2::cut_dies_after(Some(3));
    let died = cutter.settle(but(b"p"));
    alt_oplog::v2::cut_dies_after(None);
    assert!(matches!(died, Err(OpLogError::Io(_))), "{died:?}");
    std::mem::forget(cutter);
    let after = fs::read(&path).unwrap();
    assert_ne!(before[last..last + 48], after[last..last + 48]);
    // the handle leaves what it holds to its sink: the ref transaction,
    // which it reads from the file, is whole. The sink finds the place
    // cut, and the handle reads the log again for its next sync
    for _ in 0..2 {
        held.sync().unwrap();
        sink.fsync().unwrap();
    }
    assert_eq!(held.len(), 13);
    assert!(held.confirmed());
}
