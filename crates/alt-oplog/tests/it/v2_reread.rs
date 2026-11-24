//! A handle on a version 2 log that reads the log anew for a caller
//! which tells nobody that ops were cut.

use alt_oplog::{OpLog, OpLogError};

use super::v2_log::{boot, grow, new_log};

#[test]
fn a_handle_read_anew_for_a_caller_that_tells_nobody_is_told_by_its_next_sync_that_its_op_was_cut()
{
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut held = new_log(dir.path());
    grow(&mut held, "a", 2);
    held.append("a", 3, b"mine").unwrap();
    std::fs::remove_file(dir.path().join("tail")).unwrap();
    let mut other = OpLog::open(dir.path()).unwrap();
    let but_mine = |op: &alt_oplog::Op| Ok::<_, OpLogError>(op.payload != b"mine");
    assert_eq!(other.settle(but_mine).unwrap().unwrap().ops, 1);
    drop(other);
    assert!(matches!(held.follow(), Err(OpLogError::CutBack)));
    held.reread().unwrap();
    assert_eq!(held.len(), 2);
    held.append("a", 4, b"next").unwrap();
    assert!(matches!(held.sync(), Err(OpLogError::CutBack)));
    // once, and the op appended since is synced by the sync after
    held.sync().unwrap();
    assert!(held.confirmed());
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(held.appended_len()));
}
