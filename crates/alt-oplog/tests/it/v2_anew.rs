//! A handle of a version 2 log that reads the log anew, where frames it
//! held were cut or a cut of them was begun, knows still what it looked
//! at and what was appended since it opened: none of that is left to be
//! looked at for it. And it knows no more than that: what another boot
//! left is left to be looked at, on whichever side of an earlier cut of
//! the log it lies.

use alt_oplog::{Meta, OpLog, OpLogError};

use super::v2_image::linked;
use super::v2_log::{boot, grow, new_log};
use super::v2_notes::vouched;
use super::v2_unseen::seeing;

/// A handle of boot 2 that has looked at all boot 1 left, vouched for
/// it, and appended `mine` after it.
fn looked_and_appended(dir: &std::path::Path) -> OpLog {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    log.append("a", 3, b"left").unwrap();
    drop(log);
    boot(2);
    let mut held = seeing(OpLog::open(dir).unwrap());
    assert_eq!(held.settle(vouched).unwrap(), None);
    held.append("b", 4, b"mine").unwrap();
    held
}

#[test]
fn a_handle_that_reads_the_log_anew_takes_nothing_it_looked_at_or_appended_for_left_to_look_at() {
    for way in ["sync", "bound", "append"] {
        let dir = alt_os::temp::tempdir().unwrap();
        let mut held = looked_and_appended(dir.path());
        // another writer, which finds no note beside the log, begins a
        // cut of `mine` and dies before it lays its checkpoint
        std::fs::remove_file(dir.path().join("tail")).unwrap();
        let mut cutter = OpLog::open(dir.path()).unwrap();
        alt_oplog::v2::cut_dies_after(Some(1));
        let cut = cutter.settle(|op| Ok::<_, OpLogError>(op.payload != b"mine"));
        alt_oplog::v2::cut_dies_after(None);
        assert!(matches!(cut, Err(OpLogError::Io(_))), "{way}: {cut:?}");
        drop(cutter);
        // the handle reads the log anew, to its end, and holds what it
        // held: it looked at `left`, and appended `mine` itself
        let reads = held.reads();
        match way {
            "sync" => held.sync().unwrap(),
            "bound" => held.bound_now().unwrap(),
            _ => {
                assert!(matches!(held.follow(), Err(OpLogError::CutBack)));
                held.reread().unwrap();
                held.append("b", 5, b"more").unwrap();
                held.sync().unwrap();
            }
        }
        assert!(held.reads() > reads, "{way}");
        assert!(held.confirmed() && !held.unsettled(), "{way}");
        assert_eq!(held.len(), 4 + (way == "append") as usize, "{way}");
    }
}

/// A handle of boot 2 on what boot 1 left unsynced: `left`, as `meta`
/// says it, an append that was never finished and was cut off after
/// it, and `after`, appended once that was cut. Another writer of boot
/// 2 has begun a cut of the two and died before it laid its checkpoint.
/// With the handle, where `left` and `after` start.
fn past_a_cut_begun_before_an_earlier_cut(dir: &std::path::Path, meta: &Meta) -> (OpLog, [u64; 2]) {
    boot(1);
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    log.append_with("a", 3, b"left", meta).unwrap();
    let left = log.anchor().unwrap().start;
    drop(log);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join("log"));
    std::io::Write::write_all(file.as_mut().unwrap(), &[0xa5; 300]).unwrap();
    let mut log = OpLog::open(dir).unwrap();
    log.append("a", 4, b"after").unwrap();
    assert_eq!(log.state().unwrap().generation, 1);
    let after = log.anchor().unwrap().start;
    drop(log);
    boot(2);
    let held = seeing(OpLog::open(dir).unwrap());
    let mut cutter = OpLog::open(dir).unwrap();
    alt_oplog::v2::cut_dies_after(Some(1));
    let synced = |op: &alt_oplog::Op| Ok::<_, OpLogError>(op.frame.unwrap().seq <= 2);
    let cut = cutter.settle(synced);
    alt_oplog::v2::cut_dies_after(None);
    assert!(matches!(cut, Err(OpLogError::Io(_))), "{cut:?}");
    (held, [left, after])
}

#[test]
fn a_handle_that_reads_the_log_anew_takes_what_another_boot_left_for_left_on_either_side_of_a_cut()
{
    // an op a sync does not see to: `left` itself, or, where `left` is
    // a ref transaction, the op after the earlier cut
    for (unseen, meta) in [(0, Meta::default()), (1, linked())] {
        for bound in [false, true] {
            let dir = alt_os::temp::tempdir().unwrap();
            let (mut held, starts) = past_a_cut_begun_before_an_earlier_cut(dir.path(), &meta);
            let synced = match bound {
                true => held.bound_now(),
                false => held.sync(),
            };
            let refused =
                matches!(synced, Err(OpLogError::Unjudged { at }) if at == starts[unseen]);
            assert!(refused, "the op {unseen}, bound: {bound}: {synced:?}");
            assert!(held.reads() > 0 && held.unsettled() && !held.confirmed());
        }
    }
}
