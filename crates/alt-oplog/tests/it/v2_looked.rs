//! A sync of a version 2 log sets no boundary over an op another boot
//! left that nobody looked at and that the sync does not see to.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use alt_oplog::{Meta, Op, OpLog, OpLogError};

use super::v2_image::linked;
use super::v2_log::{boot, grow, new_log};
use super::v2_named::collecting;
use super::v2_reread::cut_from;

fn all(_: &Op) -> Result<bool, OpLogError> {
    Ok(true)
}

/// An op that changes the index of workspace `n`.
fn staged(n: u8) -> Meta {
    Meta {
        workspace: Some([n; 8]),
        index_after: Some([n; 20]),
        ..Meta::default()
    }
}

/// Ops by their payloads, with what their frames say of them.
type Left<'a> = Vec<(&'a [u8], Meta)>;

/// A log of two synced ops and, left unsynced after them by boot 1, the
/// ops `left`; where each of those starts.
fn left_by_boot_1(dir: &Path, left: &[(&[u8], Meta)]) -> Vec<u64> {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    let mut starts = Vec::new();
    for (payload, meta) in left {
        log.append_with("a", 3, payload, meta).unwrap();
        starts.push(log.anchor().unwrap().start);
    }
    starts
}

/// What a sync of a handle of boot 2 comes to, which is given somebody
/// to sync what ops name when `sees`, and the payloads that one was
/// given.
fn synced_in_boot_2(dir: &Path, sees: bool) -> (Result<(), OpLogError>, Vec<Vec<u8>>, OpLog) {
    boot(2);
    let mut log = OpLog::open(dir).unwrap();
    let given = sees.then(|| collecting(&mut log));
    let synced = log.sync();
    let given = given.map(|given| given.lock().unwrap().clone());
    let payloads = |ops: Vec<Op>| ops.into_iter().map(|op| op.payload).collect();
    (synced, given.map_or(Vec::new(), payloads), log)
}

#[test]
fn a_sync_sets_no_boundary_over_an_op_another_boot_left_that_it_does_not_see_to() {
    // the ops boot 1 left, and which of them is the first not seen to
    let plain = || (&b"plain"[..], Meta::default());
    let cases: [(&str, Left, usize); 3] = [
        ("an op that names nothing a sync sees to", vec![plain()], 0),
        (
            "past a ref transaction",
            vec![(b"tx", linked()), plain()],
            1,
        ),
        (
            "a change of an index that an op of its workspace replaced",
            vec![(b"first", staged(1)), (b"second", staged(1))],
            0,
        ),
    ];
    for (case, left, unseen) in cases {
        let dir = alt_os::temp::tempdir().unwrap();
        let starts = left_by_boot_1(dir.path(), &left);
        let synced = alt_oplog::bounded_in(dir.path());
        let (told, given, mut log) = synced_in_boot_2(dir.path(), true);
        match told {
            Err(OpLogError::Unjudged { at }) => assert_eq!(at, starts[unseen], "{case}"),
            other => panic!("{case}: {other:?}"),
        }
        assert!(given.is_empty(), "{case}");
        assert!(!log.confirmed(), "{case}");
        boot(1);
        assert_eq!(alt_oplog::bounded_in(dir.path()), synced, "{case}");
        boot(2);
        // once a writer has looked, the sync is made
        assert_eq!(log.settle(all).unwrap(), None, "{case}");
        log.sync().unwrap();
        assert!(log.confirmed(), "{case}");
    }
}

#[test]
fn a_sync_sees_to_what_another_boot_left_where_every_op_of_it_is_one_it_sees_to() {
    let dir = alt_os::temp::tempdir().unwrap();
    let left = [
        (&b"tx"[..], linked()),
        (b"staged", staged(1)),
        (b"another", staged(2)),
    ];
    left_by_boot_1(dir.path(), &left);
    let (told, given, log) = synced_in_boot_2(dir.path(), true);
    told.unwrap();
    assert_eq!(
        given,
        [b"tx".to_vec(), b"staged".to_vec(), b"another".to_vec()]
    );
    assert!(log.confirmed());
}

#[test]
fn a_handle_given_nobody_to_sync_what_ops_name_sets_no_boundary_over_what_another_boot_left() {
    let dir = alt_os::temp::tempdir().unwrap();
    let starts = left_by_boot_1(dir.path(), &[(b"tx", linked())]);
    let (told, _, mut log) = synced_in_boot_2(dir.path(), false);
    assert!(matches!(told, Err(OpLogError::Unjudged { at }) if at == starts[0]));
    // looked at, the op is in a log that is fsynced: nobody saw to what
    // it names, and no boundary says so
    assert_eq!(log.settle(all).unwrap(), None);
    log.sync().unwrap();
    assert!(!log.confirmed());
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(starts[0]));
}

#[test]
fn what_was_appended_since_a_handle_opened_the_log_is_none_to_look_at() {
    // by the handle itself, with no look
    let dir = alt_os::temp::tempdir().unwrap();
    left_by_boot_1(dir.path(), &[(b"tx", linked())]);
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    let given = collecting(&mut log);
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        log.append("a", 4, b"of this boot").unwrap();
    }
    log.sync().unwrap();
    assert!(log.confirmed());
    assert_eq!(given.lock().unwrap().len(), 1);

    // by another, after what was never finished at the end was cut off
    let dir = alt_os::temp::tempdir().unwrap();
    left_by_boot_1(dir.path(), &[(b"tx", linked())]);
    let path = dir.path().join("log");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend([7; 300]);
    std::fs::write(&path, bytes).unwrap();
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    let _given = collecting(&mut log);
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        let mut other = OpLog::open(dir.path()).unwrap();
        other.append("b", 4, b"of this boot").unwrap();
    }
    log.refresh().unwrap();
    assert_eq!(log.len(), 4);
    log.sync().unwrap();
    assert!(log.confirmed());
}

#[test]
fn once_somebody_has_vouched_for_all_a_handle_read_its_sync_asks_for_no_look() {
    let dir = alt_os::temp::tempdir().unwrap();
    left_by_boot_1(dir.path(), &[(b"plain", Meta::default())]);
    let (told, _, mut log) = synced_in_boot_2(dir.path(), true);
    assert!(matches!(told, Err(OpLogError::Unjudged { .. })));
    let mut other = OpLog::open(dir.path()).unwrap();
    assert_eq!(other.settle(all).unwrap(), None);
    log.sync().unwrap();
    assert!(log.confirmed());
}

#[test]
fn a_writer_whose_ops_are_cut_each_time_it_gives_up_the_lock_sets_its_boundary_once_they_are_gone()
{
    let dir = alt_os::temp::tempdir().unwrap();
    left_by_boot_1(dir.path(), &[(b"first", linked()), (b"second", linked())]);
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    // whoever syncs what the ops name finds, the first two times, that
    // another writer cuts the last of them meanwhile and that the note
    // of who looked is gone after
    let (path, calls) = (dir.path().to_owned(), Arc::new(AtomicUsize::new(0)));
    let called = calls.clone();
    log.sync_named_with(Arc::new(move |_| {
        let cut = match called.fetch_add(1, Ordering::Relaxed) {
            0 => &b"second"[..],
            1 => b"first",
            _ => return Ok(()),
        };
        assert_eq!(cut_from(&path, cut), 1);
        std::fs::remove_file(path.join("tail"))
    }));
    log.bound_now().unwrap();
    assert_eq!((log.len(), calls.load(Ordering::Relaxed)), (2, 3));
    assert!(log.confirmed());
}
