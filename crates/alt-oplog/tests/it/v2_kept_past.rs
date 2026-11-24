//! Which frames past an op a writer is refused are kept from a cut of a
//! version 2 log: every frame past it is looked at, receipts and ops
//! alike, each as the ops before it are.

use alt_oplog::{Meta, OpLog};

use super::v2_kept::{receipt_of, settled, staged};
use super::v2_log::{boot, file_len, new_log};

#[test]
fn a_receipt_a_sync_covered_is_kept_from_a_cut_though_a_receipt_before_it_is_none_to_keep() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    let mut asked = Vec::new();
    for request in [1, 2] {
        let of_request = Meta {
            request: Some([request; 16]),
            ..Meta::default()
        };
        log.append_with("a", 1, b"asked", &of_request).unwrap();
        log.sync().unwrap();
        asked.push(log.anchor().unwrap().start);
    }
    // a writer who syncs changes the index of a workspace, and the
    // receipt of the first request follows: a sync that covered it saw
    // to `left`
    log.append_with("a", 2, b"left", &staged(1)).unwrap();
    let left = log.anchor().unwrap().start;
    log.append_receipt(&receipt_of(1, asked[0])).unwrap();
    // an op of the workspace replaces `left`, and the receipt of the
    // second request follows that: the sync that covers it does not
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        log.append_with("a", 3, b"staged", &staged(1)).unwrap();
    }
    let kept = log.appended_len();
    log.append_receipt(&receipt_of(2, asked[1])).unwrap();
    log.sync().unwrap();
    assert!(log.confirmed());
    drop(log);
    let len = file_len(dir.path());
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(
        settled(&mut log),
        format!("Err(Kept {{ unheld: {left}, kept: {kept} }})")
    );
    assert_eq!(file_len(dir.path()), len);
    // without the first receipt it is the same
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    let of_request = Meta {
        request: Some([2; 16]),
        ..Meta::default()
    };
    log.append_with("a", 1, b"asked", &of_request).unwrap();
    log.sync().unwrap();
    let asked = log.anchor().unwrap().start;
    log.append_with("a", 2, b"left", &staged(1)).unwrap();
    let left = log.anchor().unwrap().start;
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        log.append_with("a", 3, b"staged", &staged(1)).unwrap();
    }
    let kept = log.appended_len();
    log.append_receipt(&receipt_of(2, asked)).unwrap();
    log.sync().unwrap();
    drop(log);
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(
        settled(&mut log),
        format!("Err(Kept {{ unheld: {left}, kept: {kept} }})")
    );
}
