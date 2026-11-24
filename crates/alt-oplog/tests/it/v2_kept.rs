//! A writer does not cut a version 2 log before a frame that a sync may
//! have covered, where nothing beside the log tells how far it was
//! synced.

use std::fs;
use std::path::Path;

use alt_oplog::v2::receipt::{Outcome, Receipt};
use alt_oplog::{Meta, OpLog, OpLogError};

use super::v2_cut_back::but_left;
use super::v2_image::linked;
use super::v2_log::{boot, file_len, grow, new_log};

/// What a look that refuses `left` comes to, as text.
pub fn settled(log: &mut OpLog) -> String {
    format!("{:?}", log.settle(but_left))
}

/// The payloads of the ops of the log in `dir`, as a writer reads it:
/// to its end, whatever a reader noted of where it ends.
fn payloads(dir: &Path) -> Vec<Vec<u8>> {
    let mut log = OpLog::open(dir).unwrap();
    log.refresh().unwrap();
    let ops = log.ops().unwrap();
    ops.iter().map(|op| op.payload.clone()).collect()
}

pub fn receipt_of(request: u8, first_op: u64) -> Receipt {
    let mut receipt = Receipt {
        request: [request; 16],
        binding: [7; 32],
        outcome: Outcome::Executed,
        exit: 0,
        first_op,
        ops: 1,
        finished_ms: 1,
        digest: [0; 32],
        len: 0,
        response: None,
    };
    receipt.answer(b"done");
    receipt
}

#[test]
fn a_transaction_another_handle_synced_is_not_cut_with_an_op_before_it_whatever_became_of_the_note()
{
    for noted in [true, false] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        log.append("a", 3, b"left").unwrap();
        let left = log.anchor().unwrap().start;
        log.append_with("a", 4, b"tx", &linked()).unwrap();
        let tx = log.anchor().unwrap().start;
        drop(log);
        boot(2);
        let mut looking = OpLog::open(dir.path()).unwrap();
        // another handle looks, finds what both name stored, and syncs
        let mut syncing = OpLog::open(dir.path()).unwrap();
        let all = |_: &alt_oplog::Op| Ok::<_, OpLogError>(true);
        assert_eq!(syncing.settle(all).unwrap(), None);
        syncing.sync().unwrap();
        let end = syncing.appended_len();
        assert_eq!(alt_oplog::bounded_in(dir.path()), Some(end));
        if !noted {
            fs::remove_file(dir.path().join("tail")).unwrap();
        }
        let said = settled(&mut looking);
        let refused = match noted {
            true => format!("Err(Lost {{ synced: {end}, holds: {left} }})"),
            false => format!("Err(Kept {{ unheld: {left}, kept: {tx} }})"),
        };
        assert_eq!(said, refused, "noted: {noted}");
        assert_eq!(file_len(dir.path()), end);
        assert_eq!(payloads(dir.path()).len(), 4);
    }
}

#[test]
fn a_commit_that_was_answered_before_the_power_went_is_not_cut_with_an_op_refused_before_it() {
    for noted_since in [false, true] {
        answered_before_the_power_went(noted_since);
    }
}

/// With `noted_since`, a reader of the boot after has left a note beside
/// the log before the look: it knows of no sync of the boot before.
fn answered_before_the_power_went(noted_since: bool) {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        log.append("a", 3, b"left").unwrap();
    }
    let left = log.anchor().unwrap().start;
    log.append_with("a", 4, b"commit", &linked()).unwrap();
    let commit = log.anchor().unwrap().start;
    // the command is answered: the log is synced past its frame, which
    // no frame says yet
    log.sync().unwrap();
    assert!(log.confirmed());
    drop(log);
    let len = file_len(dir.path());
    // the power went, and of what `left` names a part is gone
    boot(2);
    if noted_since {
        let reader = OpLog::open_read_only(dir.path(), None).unwrap();
        assert_eq!(reader.unconfirmed(but_left).unwrap().unwrap().ops, 2);
        let note = fs::read(dir.path().join("tail")).unwrap();
        assert_eq!(note[8..24], [2; 16], "a note of this boot");
    }
    let mut log = OpLog::open(dir.path()).unwrap();
    let said = settled(&mut log);
    assert_eq!(
        said,
        format!("Err(Kept {{ unheld: {left}, kept: {commit} }})"),
        "noted since: {noted_since}"
    );
    assert_eq!(file_len(dir.path()), len);
    assert!(payloads(dir.path()).contains(&b"commit".to_vec()));
    // nor does the next handle cut it, which is refused the same
    let mut next = OpLog::open(dir.path()).unwrap();
    assert_eq!(settled(&mut next), said);
}

#[test]
fn a_receipt_is_kept_from_a_cut_as_the_ops_of_its_request_are() {
    // the receipt of a request whose op is synced lies past `left`
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    let of_request = Meta {
        request: Some([1; 16]),
        ..Meta::default()
    };
    log.append_with("a", 1, b"asked", &of_request).unwrap();
    log.sync().unwrap();
    let asked = log.anchor().unwrap().start;
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        log.append("a", 2, b"left").unwrap();
    }
    let left = log.anchor().unwrap().start;
    let kept = log.appended_len();
    log.append_receipt(&receipt_of(1, asked)).unwrap();
    log.sync().unwrap();
    drop(log);
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(
        settled(&mut log),
        format!("Err(Kept {{ unheld: {left}, kept: {kept} }})")
    );
    assert_eq!(payloads(dir.path()).len(), 2);

    // the receipt of a request whose op is the one refused goes with it
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    let of_request = Meta {
        request: Some([2; 16]),
        ..Meta::default()
    };
    log.append_with("a", 3, b"left", &of_request).unwrap();
    let left = log.anchor().unwrap().start;
    log.append_receipt(&receipt_of(2, left)).unwrap();
    drop(log);
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.settle(but_left).unwrap().unwrap().ops, 1);
    assert_eq!(payloads(dir.path()).len(), 2);
}

#[test]
fn ops_past_the_one_refused_that_no_writer_who_syncs_left_or_whose_content_is_gone_are_cut_with_it()
{
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    log.append("a", 3, b"left").unwrap();
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        log.append("a", 4, b"staged").unwrap();
    }
    log.append_with("a", 5, b"dangling", &linked()).unwrap();
    drop(log);
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    let held = |op: &alt_oplog::Op| {
        Ok::<_, OpLogError>(op.payload != b"left" && op.payload != b"dangling")
    };
    assert_eq!(log.settle(held).unwrap().unwrap().ops, 3);
    assert_eq!(payloads(dir.path()).len(), 2);
}

/// An op that changes the index of workspace `n`.
pub fn staged(n: u8) -> Meta {
    Meta {
        workspace: Some([n; 8]),
        index_after: Some([n; 20]),
        ..Meta::default()
    }
}

/// What a look that refuses `left` comes to on a log of two synced ops,
/// `left` as `refused` says it, and an op after it as `after` does, both
/// left by another boot and by writers that sync.
fn left_and_after(refused: &Meta, after: &Meta) -> (String, u64, u64) {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    log.append_with("a", 3, b"left", refused).unwrap();
    let left = log.anchor().unwrap().start;
    log.append_with("a", 4, b"after", after).unwrap();
    let after = log.anchor().unwrap().start;
    drop(log);
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    (settled(&mut log), left, after)
}

#[test]
fn a_cut_takes_what_follows_an_op_that_a_sync_past_it_would_have_seen_to() {
    // a ref transaction, and the op of a workspace that last changed its
    // index: a sync that covered a frame past either saw to it first,
    // and what it names would be stored
    let cases = [
        ("a ref transaction", linked(), linked()),
        ("the last change of an index", staged(1), linked()),
        ("with another workspace changed after", staged(1), staged(2)),
    ];
    for (case, refused, after) in cases {
        let (said, left, _) = left_and_after(&refused, &after);
        let cut = format!("Ok(Some(Unconfirmed {{ at: {left}, ops: 2 }}))");
        assert_eq!(said, cut, "{case}");
    }
}

#[test]
fn a_change_of_an_index_that_its_workspace_replaced_is_not_cut_with_what_a_sync_may_have_covered() {
    let retiring = Meta {
        workspace: Some([1; 8]),
        retired: true,
        ..Meta::default()
    };
    for (case, after) in [("replaced", staged(1)), ("the workspace gone", retiring)] {
        let (said, left, after) = left_and_after(&staged(1), &after);
        let kept = format!("Err(Kept {{ unheld: {left}, kept: {after} }})");
        assert_eq!(said, kept, "{case}");
    }
}
