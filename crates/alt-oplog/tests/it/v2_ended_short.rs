//! A handle of a version 2 log for which the log ended short of the file,
//! where a cut was begun and never made, syncs: no op past that place
//! is taken for looked at by it.

use std::sync::Arc;

use alt_oplog::{OpLog, OpLogError};

use super::v2_cuts::but_gone;
use super::v2_image::linked;
use super::v2_kept::staged;
use super::v2_log::{boot, grow, new_log};
use super::v2_unseen::seeing;

/// A log as boot 4 finds it: two synced ops and a commit that boot 1
/// synced, a change of an index `gone` that a writer of boot 2 left
/// unsynced, and in its header a cut of that op which boot 3 began and
/// died of before it laid its checkpoint.
fn with_a_cut_begun(dir: &std::path::Path) {
    boot(1);
    let mut log = seeing(new_log(dir));
    grow(&mut log, "a", 2);
    log.append_with("a", 3, b"committed", &linked()).unwrap();
    log.sync().unwrap();
    drop(log);
    boot(2);
    let mut log = OpLog::open(dir).unwrap();
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        log.append_with("b", 4, b"gone", &staged(1)).unwrap();
    }
    drop(log);
    boot(3);
    let mut cutter = OpLog::open(dir).unwrap();
    alt_oplog::v2::cut_dies_after(Some(1));
    let cut = cutter.settle(but_gone);
    alt_oplog::v2::cut_dies_after(None);
    assert!(matches!(cut, Err(OpLogError::Io(_))), "{cut:?}");
    drop(cutter);
    boot(4);
}

#[test]
fn a_sync_of_a_handle_the_log_ended_short_for_leaves_what_lies_past_there_to_be_looked_at() {
    for deferred in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        with_a_cut_begun(dir.path());
        let mut held = OpLog::open(dir.path()).unwrap();
        // whoever syncs what ops name finds what `gone` names not there
        held.sync_named_with(Arc::new(|ops| {
            match ops.iter().any(|op| op.payload == b"gone") {
                true => Err(std::io::Error::other("not there")),
                false => Ok(()),
            }
        }));
        held.set_defer_durability(deferred);
        let sink = held.sync_handle().unwrap();
        assert_eq!(held.len(), 3, "deferred: {deferred}");
        let synced = held.sync().and_then(|()| sink.fsync());
        // it reads the log to its end first, and sets no boundary over
        // the op nobody could see to
        assert!(
            matches!(synced, Err(OpLogError::Io(_))),
            "deferred: {deferred}, {synced:?}"
        );
        assert!(!held.confirmed(), "deferred: {deferred}");
        drop((held, sink));
        let mut next = OpLog::open(dir.path()).unwrap();
        assert!(next.unsettled(), "deferred: {deferred}");
        let cut = next.settle(but_gone).unwrap();
        assert_eq!(cut.map(|cut| cut.ops), Some(1), "deferred: {deferred}");
        assert_eq!(next.len(), 3);
    }
}
