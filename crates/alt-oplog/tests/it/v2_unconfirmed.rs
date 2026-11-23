//! Frames of a version 2 log that another boot left unsynced: a writer
//! cuts the log from the first op on that names content which is not
//! stored, a reader is told where it starts, and what was cut is told.
//! In the boot that wrote them there is nothing to look at.

use std::fs;
use std::path::Path;

use alt_oplog::v2::frame::{Cut, KIND_CHECKPOINT};
use alt_oplog::{Anchor, Op, OpLog, OpLogError, Unconfirmed};

use super::v2_log::{boot, file_len, frames, grow, new_log};

/// In boot 1: three synced ops, then `unsynced` appended and left so.
/// Returns the anchor of each of those.
fn left_by_boot_1(dir: &Path, unsynced: &[&[u8]]) -> Vec<Anchor> {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "synced", 3);
    let mut anchors = Vec::new();
    for payload in unsynced {
        log.append("actor", 9, payload).unwrap();
        anchors.push(log.anchor().unwrap());
    }
    anchors
}

fn held(op: &Op) -> Result<bool, OpLogError> {
    Ok(!op.payload.starts_with(b"gone"))
}

fn payloads(ops: &[Op]) -> Vec<&[u8]> {
    ops.iter().map(|op| &op.payload[..]).collect()
}

#[test]
fn a_writer_cuts_from_the_first_op_another_boot_left_that_names_what_is_not_stored() {
    let dir = alt_os::temp::tempdir().unwrap();
    let left = left_by_boot_1(dir.path(), &[b"kept", b"gone", b"after it"]);
    let len = file_len(dir.path());
    // the boot that wrote them has nothing to look at
    let mut log = OpLog::open(dir.path()).unwrap();
    assert!(!log.unsettled() && !log.confirmed());
    assert_eq!(log.settle(held).unwrap(), None);
    assert_eq!(log.left_unsynced_from(), Some(left[0].start));
    drop(log);

    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert!(log.unsettled());
    assert_eq!(log.left_unsynced_from(), None);
    let found = Unconfirmed {
        at: left[1].start,
        ops: 2,
    };
    assert_eq!(log.settle(held).unwrap(), Some(found));
    assert_eq!((log.len(), log.reads()), (4, 1));
    assert_eq!(log.recent().last().unwrap().payload, b"kept");
    let cut = log.take_discarded();
    assert_eq!((cut[0].from, cut[0].to, cut[0].unread), (found.at, len, 0));
    assert_eq!(payloads(&cut[0].ops), [&b"gone"[..], b"after it"]);
    assert!(log.take_discarded().is_empty());
    // the log says so where it was cut, and is synced
    let (all, state) = frames(dir.path());
    let last = all.last().unwrap();
    let bytes = len - found.at;
    assert_eq!((last.0, last.2), (found.at, KIND_CHECKPOINT));
    let first = Cut {
        bytes,
        ops: 2,
        after: 0,
        generation: 0,
    };
    assert_eq!(last.3, Some(first));
    assert_eq!((state.generation, state.seq), (1, 4));

    // nothing more to look at, by this handle or the next of this boot
    assert!(!log.unsettled());
    assert_eq!(log.settle(held).unwrap(), None);
    log.append("actor", 10, b"anew").unwrap();
    log.sync().unwrap();
    let log = OpLog::open(dir.path()).unwrap();
    assert!(!log.unsettled() && log.confirmed());
    log.verify().unwrap();
}

#[test]
fn an_op_within_what_is_synced_is_not_looked_at() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    for payload in [&b"a"[..], b"gone but synced", b"c"] {
        log.append("actor", 1, payload).unwrap();
        log.sync().unwrap();
    }
    let synced = log.appended_len();
    log.append("actor", 2, b"kept").unwrap();
    drop(log);
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.bounded_to(), Some(synced));
    let mut asked = Vec::new();
    let cut = log.settle(|op| {
        asked.push(op.payload.clone());
        Ok::<_, OpLogError>(false)
    });
    // the one op past the boundary is asked about, and cut
    assert_eq!(asked, [b"kept"]);
    assert_eq!(cut.unwrap().unwrap().at, synced);
    assert_eq!(log.len(), 3);
}

#[test]
fn a_reader_is_told_where_the_ops_start_and_the_next_reader_ends_there() {
    let dir = alt_os::temp::tempdir().unwrap();
    let left = left_by_boot_1(dir.path(), &[b"kept", b"gone", b"after it"]);
    boot(2);
    let before = fs::read(dir.path().join("log")).unwrap();
    let log = OpLog::open_read_only(dir.path(), None).unwrap();
    assert_eq!(log.len(), 6);
    let mut asked = Vec::new();
    let found = log.unconfirmed_past(|op, left| {
        asked.push((op.payload.clone(), left));
        held(op)
    });
    let at = left[1].start;
    assert_eq!(found.unwrap(), Some(Unconfirmed { at, ops: 2 }));
    assert_eq!(asked.len(), 2, "the ops past the boundary, up to the first");
    assert!(!asked[0].1, "their writer was one that syncs");
    assert_eq!(fs::read(dir.path().join("log")).unwrap(), before);

    // the next reader of this boot takes the log to end there, unasked
    let log = OpLog::open_read_only(dir.path(), None).unwrap();
    assert_eq!((log.len(), log.appended_len()), (4, at));
    assert_eq!(
        log.bounded_to(),
        Some(left[0].start),
        "as the frames it left out said"
    );
    assert!(log.passed_over());
    assert_eq!(
        payloads(&log.passed_ops().unwrap()),
        [&b"gone"[..], b"after it"]
    );
    let none = log.unconfirmed(|_| panic!("nothing is left to ask about"));
    assert_eq!(none.unwrap_or_else(|e: OpLogError| panic!("{e}")), None);
    // a writer still has them to cut
    let mut log = OpLog::open(dir.path()).unwrap();
    assert!(log.unsettled());
    assert_eq!(log.settle(held).unwrap(), Some(Unconfirmed { at, ops: 2 }));
    assert_eq!(file_len(dir.path()), log.appended_len());

    // where the first op left is the one, no frame still read says how
    // far the log is synced: the note does
    let dir = alt_os::temp::tempdir().unwrap();
    let left = left_by_boot_1(dir.path(), &[b"gone"]);
    boot(2);
    let log = OpLog::open_read_only(dir.path(), None).unwrap();
    assert!(log.unconfirmed(held).unwrap().is_some());
    let log = OpLog::open_read_only(dir.path(), None).unwrap();
    assert_eq!((log.len(), log.bounded_to()), (3, Some(left[0].start)));
    assert!(log.confirmed() && log.unsettled());

    // where every op is vouched for, a reader notes that and nothing is asked again
    let dir = alt_os::temp::tempdir().unwrap();
    left_by_boot_1(dir.path(), &[b"kept", b"kept too"]);
    boot(2);
    let log = OpLog::open_read_only(dir.path(), None).unwrap();
    assert!(log.unsettled());
    assert_eq!(log.unconfirmed(held).unwrap(), None);
    assert!(!log.unsettled());
    let log = OpLog::open(dir.path()).unwrap();
    assert!(!log.unsettled() && !log.confirmed());
    assert_eq!(log.len(), 5);
}

#[test]
fn the_log_ends_at_the_first_frame_left_unsynced_that_does_not_read_in_any_boot() {
    for later in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let left = left_by_boot_1(dir.path(), &[b"first", b"damaged", b"whole"]);
        let len = file_len(dir.path());
        let path = dir.path().join("log");
        let mut bytes = fs::read(&path).unwrap();
        bytes[left[1].start as usize + 70] ^= 1;
        fs::write(&path, bytes).unwrap();
        if later {
            boot(2);
        }
        for read_only in [true, false] {
            let log = match read_only {
                true => OpLog::open_read_only(dir.path(), None).unwrap(),
                false => OpLog::open(dir.path()).unwrap(),
            };
            assert_eq!((log.len(), log.appended_len()), (4, left[1].start));
            assert!(log.passed_over());
            // the whole frame after it is found, and goes with it
            assert_eq!(payloads(&log.passed_ops().unwrap()), [b"whole"]);
            assert_eq!(
                log.passed().unwrap(),
                (left[1].start, len, Some(left[2].start))
            );
            assert_eq!(file_len(dir.path()), len);
        }
        let mut log = OpLog::open(dir.path()).unwrap();
        log.append("actor", 1, b"next").unwrap();
        let cut = log.take_discarded();
        assert_eq!((cut[0].from, cut[0].to), (left[1].start, len));
        assert_eq!(payloads(&cut[0].ops), [b"whole"]);
        assert_eq!(cut[0].unread, left[1].end - left[1].start, "{later}");
        assert_eq!(log.len(), 5);
        log.verify().unwrap();
    }
}

#[test]
fn a_handle_whose_op_was_cut_is_refused_until_it_reads_the_log_again() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    grow(&mut new_log(dir.path()), "synced", 3);
    let mut victim = OpLog::open(dir.path()).unwrap();
    victim.append("victim", 1, b"gone").unwrap();
    // a writer that takes the op for another boot's cuts it
    boot(2);
    let mut writer = OpLog::open(dir.path()).unwrap();
    assert!(writer.settle(held).unwrap().is_some());
    writer.append("writer", 2, b"instead").unwrap();
    writer.sync().unwrap();

    assert!(matches!(victim.sync(), Err(OpLogError::CutBack)));
    assert!(matches!(
        victim.append("victim", 3, b"x"),
        Err(OpLogError::CutBack)
    ));
    assert!(matches!(victim.refresh(), Err(OpLogError::CutBack)));
    assert!(matches!(victim.follow(), Err(OpLogError::CutBack)));
    victim.reopen().unwrap();
    assert_eq!((victim.reads(), victim.len()), (1, 4));
    assert_eq!(victim.recent().last().unwrap().payload, b"instead");
    victim.append("victim", 3, b"again").unwrap();
    victim.sync().unwrap();
    OpLog::open(dir.path()).unwrap().verify().unwrap();
}

#[test]
fn a_handle_that_reads_the_history_after_its_ops_were_cut_is_told_they_were_cut() {
    for appended_after in [true, false] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        let synced = grow(&mut log, "synced", 3);
        let _unsynced = alt_os::durability::leave_unsynced(true);
        for n in 0..60u8 {
            let payload = if n == 20 {
                vec![b'g', b'o', b'n', b'e']
            } else {
                vec![n; 150]
            };
            log.append("actor", 9, &payload).unwrap();
        }
        drop(log);
        // it holds the ops of the last block, and reads the others when asked
        let held_open = OpLog::open(dir.path()).unwrap();
        assert!(held_open.recent().len() < 30 && file_len(dir.path()) > 2 * 4096);
        let head = held_open.head().unwrap();
        boot(2);
        let mut writer = OpLog::open(dir.path()).unwrap();
        assert_eq!(writer.settle(held).unwrap().unwrap().ops, 40);
        if appended_after {
            // the file is as long again as the handle knew it
            for n in 0..60u8 {
                writer.append("writer", 2, &[n; 150]).unwrap();
            }
        }
        let cut_back = |read: Result<(), OpLogError>| {
            assert!(matches!(read, Err(OpLogError::CutBack)), "{read:?}");
        };
        cut_back(held_open.ops().map(|_| ()));
        cut_back(held_open.get(&head).map(|_| ()));
        cut_back(held_open.since(&synced[0]).map(|_| ()));
        cut_back(held_open.verify().map(|_| ()));
        cut_back(held_open.passed().map(|_| ()));
        cut_back(held_open.passed_ops().map(|_| ()));
        let mut held_open = held_open;
        held_open.reopen().unwrap();
        let expected = if appended_after { 83 } else { 23 };
        assert_eq!(held_open.ops().unwrap().len(), expected);
        held_open.verify().unwrap();
    }
}

#[test]
fn a_handle_whose_op_stayed_goes_on_after_a_cut_behind_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    grow(&mut new_log(dir.path()), "synced", 3);
    let mut kept = OpLog::open(dir.path()).unwrap();
    kept.append("kept", 1, b"kept").unwrap();
    let end = kept.appended_len();
    OpLog::open(dir.path())
        .unwrap()
        .append("other", 2, b"gone")
        .unwrap();
    boot(2);
    let mut writer = OpLog::open(dir.path()).unwrap();
    assert_eq!(writer.settle(held).unwrap().unwrap().at, end);
    drop(writer);

    kept.sync().unwrap();
    assert_eq!(kept.synced_here(), end);
    kept.append("kept", 3, b"next").unwrap();
    assert_eq!(kept.state().unwrap().generation, 1, "it read what was cut");
    assert_eq!(kept.len(), 5);
    kept.sync().unwrap();
    kept.verify().unwrap();
}

#[test]
fn a_log_that_ends_short_of_what_was_synced_is_refused_by_a_writer() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = new_log(dir.path());
    let anchors = grow(&mut log, "a", 3);
    drop(log);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("log"))
        .unwrap();
    file.set_len(anchors[1].end).unwrap();
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 2);
    let lost = |said: Result<(), OpLogError>| match said {
        Err(OpLogError::Lost { synced, holds }) => {
            assert_eq!((synced, holds), (anchors[2].end, anchors[1].end))
        }
        other => panic!("{other:?}"),
    };
    lost(log.check_synced());
    lost(log.append("a", 1, b"x").map(|_| ()));
    lost(log.settle(held).map(|_| ()));
    assert_eq!(file_len(dir.path()), anchors[1].end);
}

#[test]
fn a_panic_under_the_lock_gives_the_lock_up() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 1);
    log.append("a", 1, b"left").unwrap();
    let appended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = log.append_checked("a", 2, |_| -> Result<Vec<u8>, OpLogError> {
            panic!("in the check of an append")
        });
    }));
    assert!(appended.is_err());
    boot(2);
    let mut other = OpLog::open(dir.path()).unwrap();
    let settled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = other.settle(|_| -> Result<bool, OpLogError> { panic!("in what vouches") });
    }));
    assert!(settled.is_err());
    // both locks were given up: another handle appends
    let mut third = OpLog::open(dir.path()).unwrap();
    third.append("b", 3, b"x").unwrap();
    assert_eq!(third.len(), 3);
}
