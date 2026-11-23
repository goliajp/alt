//! A handle on a version 2 log is told that its ops were cut when they
//! were, and only then.

use super::v2_log::{boot, new_log};

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
