//! Which frames past an op a writer is refused are kept from a cut of a
//! version 2 log: every frame past it is looked at, receipts and ops
//! alike, each as the ops before it are. One is kept that a sync may
//! have covered, and none is past an op such a sync would have seen to
//! that names what is not stored.

use alt_oplog::{Meta, OpLog, OpLogError};

use super::v2_image::linked;
use super::v2_kept::{receipt_of, settled, staged};
use super::v2_log::{boot, file_len, grow, new_log};

#[test]
fn a_receipt_a_sync_covered_is_kept_from_a_cut_wherever_past_the_op_refused_it_lies() {
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
    // receipt of the first request follows
    log.append_with("a", 2, b"left", &staged(1)).unwrap();
    let left = log.anchor().unwrap().start;
    let kept = log.appended_len();
    log.append_receipt(&receipt_of(1, asked[0])).unwrap();
    // an op of the workspace replaces `left`, and the receipt of the
    // second request follows that: the sync that covers both receipts
    // sees to that op, and not to `left`
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        log.append_with("a", 3, b"staged", &staged(1)).unwrap();
    }
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
    // without the first receipt it is the second that is kept
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

/// What a look comes to that refuses the ops of the payloads `refused`,
/// on a log of two synced ops and, left after them by another boot and
/// by writers that sync, the ops `left`; with where each of those starts.
fn left_by_another_boot(left: &[(&[u8], Meta)], refused: &[&[u8]]) -> (String, Vec<u64>) {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    let mut starts = Vec::new();
    for (payload, meta) in left {
        log.append_with("a", 3, payload, meta).unwrap();
        starts.push(log.anchor().unwrap().start);
    }
    drop(log);
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    let held = |op: &alt_oplog::Op| Ok::<_, OpLogError>(!refused.contains(&&op.payload[..]));
    (format!("{:?}", log.settle(held)), starts)
}

#[test]
fn nothing_is_kept_past_an_op_a_sync_would_have_seen_to_that_names_what_is_not_stored() {
    let plain = Meta::default;
    // a ref transaction that names what is not stored: no sync covered
    // a frame past it, whatever the op refused before it is
    let (said, starts) = left_by_another_boot(
        &[
            (b"left", plain()),
            (b"dangling", linked()),
            (b"tx", linked()),
        ],
        &[b"left", b"dangling"],
    );
    let cut = format!("Ok(Some(Unconfirmed {{ at: {}, ops: 3 }}))", starts[0]);
    assert_eq!(said, cut, "a ref transaction");
    // so with the op of a workspace that last changed its index
    let (said, starts) = left_by_another_boot(
        &[
            (b"left", plain()),
            (b"dangling", staged(1)),
            (b"tx", linked()),
        ],
        &[b"left", b"dangling"],
    );
    let cut = format!("Ok(Some(Unconfirmed {{ at: {}, ops: 3 }}))", starts[0]);
    assert_eq!(said, cut, "the last change of an index");
    // and with a frame between the two that would be one to keep
    let (said, starts) = left_by_another_boot(
        &[
            (b"left", plain()),
            (b"stored", plain()),
            (b"dangling", linked()),
        ],
        &[b"left", b"dangling"],
    );
    let kept = format!("Err(Kept {{ unheld: {}, kept: {} }})", starts[0], starts[1]);
    assert_eq!(said, kept, "a frame before the ref transaction");
}

#[test]
fn a_frame_is_kept_past_a_change_of_an_index_that_names_what_is_not_stored_once_that_is_replaced() {
    // a sync that covers `tx` sees to the second change of the index,
    // and not to the first
    let (said, starts) = left_by_another_boot(
        &[
            (b"left", Meta::default()),
            (b"dangling", staged(1)),
            (b"replaces", staged(1)),
            (b"tx", linked()),
        ],
        &[b"left", b"dangling"],
    );
    let kept = format!("Err(Kept {{ unheld: {}, kept: {} }})", starts[0], starts[2]);
    assert_eq!(said, kept);
    // while the one that replaces it names what is not stored either,
    // no sync covered what follows
    let (said, starts) = left_by_another_boot(
        &[
            (b"left", Meta::default()),
            (b"dangling", staged(1)),
            (b"replaces", staged(1)),
            (b"tx", linked()),
        ],
        &[b"left", b"dangling", b"replaces"],
    );
    let cut = format!("Ok(Some(Unconfirmed {{ at: {}, ops: 4 }}))", starts[0]);
    assert_eq!(said, cut);
}

#[test]
fn a_frame_is_kept_past_an_op_that_took_its_workspace_away_and_names_what_is_not_stored() {
    // with the workspace gone no sync sees to the op, though it says
    // what closes the index
    let gone = Meta {
        retired: true,
        ..staged(1)
    };
    let (said, starts) = left_by_another_boot(&[(b"left", gone), (b"tx", linked())], &[b"left"]);
    let kept = format!("Err(Kept {{ unheld: {}, kept: {} }})", starts[0], starts[1]);
    assert_eq!(said, kept);
}

#[test]
fn a_commit_between_a_change_of_an_index_and_the_op_that_replaced_it_is_kept_from_a_cut() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    log.append_with("a", 3, b"left", &staged(1)).unwrap();
    let left = log.anchor().unwrap().start;
    log.append_with("a", 4, b"commit", &linked()).unwrap();
    let commit = log.anchor().unwrap().start;
    // a writer that does not sync changes the index again, and the
    // commit is answered by a sync that ends past that op: it saw to
    // the commit and to that op, and not to `left`
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        log.append_with("a", 5, b"staged", &staged(1)).unwrap();
    }
    log.sync().unwrap();
    assert!(log.confirmed());
    drop(log);
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(
        settled(&mut log),
        format!("Err(Kept {{ unheld: {left}, kept: {commit} }})")
    );
}

#[test]
fn a_change_of_an_index_keeps_no_frame_from_a_cut_where_what_replaced_it_may_have_been_cut() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    log.append("a", 3, b"left").unwrap();
    let left = log.anchor().unwrap().start;
    log.append_with("a", 4, b"staged", &staged(1)).unwrap();
    log.append_with("a", 5, b"commit", &linked()).unwrap();
    let commit = log.anchor().unwrap().start;
    // an op that replaces the change of the index, by a writer that
    // does not sync; the commit is answered by a sync past it
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        log.append_with("a", 6, b"again", &staged(1)).unwrap();
    }
    log.sync().unwrap();
    drop(log);
    // the op that replaced it is cut: nobody was answered for it
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    let but_again = |op: &alt_oplog::Op| Ok::<_, OpLogError>(op.payload != b"again");
    assert_eq!(log.settle(but_again).unwrap().unwrap().ops, 1);
    drop(log);
    // with what the change of the index names gone as well, it is the
    // last of its workspace in the log: the commit is kept all the same
    boot(3);
    let mut log = OpLog::open(dir.path()).unwrap();
    let held =
        |op: &alt_oplog::Op| Ok::<_, OpLogError>(op.payload != b"left" && op.payload != b"staged");
    let said = format!("{:?}", log.settle(held));
    assert_eq!(
        said,
        format!("Err(Kept {{ unheld: {left}, kept: {commit} }})")
    );
}
