//! A handle on a version 2 log is told that its ops were cut when they
//! were, and only then.

use alt_oplog::{Meta, Op, OpLog, OpLogError};

use super::v2_bounds::stage;
use super::v2_log::{boot, grow, new_log};
use super::v2_named::collecting;

fn but_left(op: &Op) -> Result<bool, OpLogError> {
    Ok(op.payload != b"left")
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
fn a_handle_whose_ops_another_cut_is_told_so_when_it_syncs_and_not_that_the_log_is_damaged() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        let linked = Meta {
            linked: true,
            ..Meta::default()
        };
        log.append_with("a", 3, b"left", &linked).unwrap();
        drop(log);
        boot(2);
        // a writer that looks at nothing appends over blocks: a handle
        // opened after begins at a checkpoint of this boot, past `left`
        let mut staging = OpLog::open(dir.path()).unwrap();
        stage(&mut staging, 40);
        drop(staging);
        let mut held = OpLog::open(dir.path()).unwrap();
        assert!(held.recent().iter().all(|op| op.payload != b"left"));
        let _given = collecting(&mut held);
        held.set_defer_durability(deferred);
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
