//! Where a sync of a version 2 log can have ended, past an op a writer
//! is refused: a frame is kept from a cut when a sync can have ended at
//! it or past it, and no other. A sync that ended at an op saw to the
//! ref transactions up to there and to the op of each workspace that
//! last changed its index as the log stood there.

use alt_oplog::{Meta, Op, OpLog, OpLogError};

use super::v2_image::linked;
use super::v2_kept::staged;
use super::v2_kept_past::left_by_another_boot;
use super::v2_log::{boot, grow, new_log};

/// An op with which workspace `n` is gone.
fn retiring(n: u8) -> Meta {
    Meta {
        workspace: Some([n; 8]),
        retired: true,
        ..Meta::default()
    }
}

#[test]
fn nothing_is_kept_past_a_change_of_an_index_that_only_an_op_which_names_nothing_stored_replaced() {
    // a sync that ended before the change which replaced the one
    // refused saw to the one refused, and a sync that ended past it saw
    // to that change: neither names what is stored, and no sync
    // covered the commit between them
    let (said, starts) = left_by_another_boot(
        &[
            (b"left", staged(1)),
            (b"tx", linked()),
            (b"dangling", staged(1)),
        ],
        &[b"left", b"dangling"],
    );
    let cut = format!("Ok(Some(Unconfirmed {{ at: {}, ops: 3 }}))", starts[0]);
    assert_eq!(said, cut);
    // with what the change after names stored, a sync that ended past
    // it covered the commit
    let (said, starts) = left_by_another_boot(
        &[
            (b"left", staged(1)),
            (b"tx", linked()),
            (b"again", staged(1)),
        ],
        &[b"left"],
    );
    let kept = format!("Err(Kept {{ unheld: {}, kept: {} }})", starts[0], starts[1]);
    assert_eq!(said, kept);
    // a sync that ended before a ref transaction which names nothing
    // stored covered what lies before that: an op left there is kept
    let (said, starts) = left_by_another_boot(
        &[
            (b"left", staged(1)),
            (b"again", staged(1)),
            (b"tx", linked()),
            (b"dangling", linked()),
        ],
        &[b"left", b"dangling"],
    );
    let kept = format!("Err(Kept {{ unheld: {}, kept: {} }})", starts[0], starts[1]);
    assert_eq!(said, kept);
}

#[test]
fn a_frame_is_kept_past_changes_of_indexes_that_name_nothing_stored_once_their_workspaces_are_gone()
{
    // a sync that ended once both workspaces were gone saw to neither
    // change: it covered the op that took the first away
    let (said, starts) = left_by_another_boot(
        &[
            (b"left", Meta::default()),
            (b"dangling", staged(2)),
            (b"dangling too", staged(1)),
            (b"gone", retiring(2)),
            (b"gone too", retiring(1)),
        ],
        &[b"left", b"dangling", b"dangling too"],
    );
    let kept = format!("Err(Kept {{ unheld: {}, kept: {} }})", starts[0], starts[3]);
    assert_eq!(said, kept);
}

/// Vouches for every op but the ones of the payloads `gone`.
fn but<'a>(gone: &'a [&'a [u8]]) -> impl FnMut(&Op) -> Result<bool, OpLogError> + 'a {
    move |op| Ok(!gone.contains(&&op.payload[..]))
}

#[test]
fn a_frame_before_a_cut_that_took_ops_is_kept_whatever_was_appended_and_cut_after_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    log.append("a", 3, b"left").unwrap();
    log.append_with("a", 3, b"dangling", &staged(1)).unwrap();
    log.append("a", 3, b"tx").unwrap();
    let kept = log.anchor().unwrap().start;
    log.append_with("a", 3, b"gone", &retiring(1)).unwrap();
    drop(log);
    // a sync that ended at the op with which the workspace was gone saw
    // to no change of its index, and covered `tx`. That op is cut, and
    // a ref transaction that names nothing stored is appended after
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.settle(but(&[b"gone"])).unwrap().unwrap().ops, 1);
    log.append_with("b", 4, b"a ref transaction", &linked())
        .unwrap();
    log.append("b", 4, b"cut too").unwrap();
    drop(log);
    // and the log is cut once more, past that transaction
    boot(3);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.settle(but(&[b"cut too"])).unwrap().unwrap().ops, 1);
    drop(log);
    boot(4);
    let mut log = OpLog::open(dir.path()).unwrap();
    let refused = log.settle(but(&[b"left", b"dangling", b"a ref transaction"]));
    let unheld = match refused {
        Err(OpLogError::Kept {
            unheld,
            kept: frame,
        }) if frame == kept => unheld,
        other => panic!("{other:?}"),
    };
    assert!(unheld < kept);
    assert_eq!(log.len(), 6);
}

#[test]
fn a_cut_of_what_a_writer_left_unfinished_keeps_no_frame_before_it() {
    for lost in ["an append", "a stretch before an op"] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        log.append_with("a", 3, b"left", &staged(1)).unwrap();
        let left = log.anchor().unwrap().start;
        log.append("a", 3, b"tx").unwrap();
        // what follows is no log: an append that was never finished,
        // or an op the power went before, a stretch of it lost, with an
        // op after it that reads
        match lost {
            "an append" => {
                drop(log);
                let path = dir.path().join("log");
                let mut file = std::fs::OpenOptions::new().append(true).open(path);
                std::io::Write::write_all(file.as_mut().unwrap(), &[0xa5; 300]).unwrap();
            }
            _ => {
                log.append("a", 3, &[7; 300]).unwrap();
                let torn = log.anchor().unwrap();
                log.append("a", 3, b"reads").unwrap();
                drop(log);
                super::v2_bounds::zero(dir.path(), torn.start + 100, torn.start + 200);
            }
        }
        boot(2);
        let mut log = OpLog::open(dir.path()).unwrap();
        log.refresh().unwrap();
        let cut = log.take_discarded();
        let read = (lost != "an append") as usize;
        assert_eq!((cut.len(), cut[0].ops.len()), (1, read), "{lost}");
        // no sync ended among what that cut took: every sync that
        // covered `tx` saw to `left` first
        let cut = log.settle(but(&[b"left"])).unwrap().unwrap();
        assert_eq!((cut.at, cut.ops), (left, 2), "{lost}");
    }
}

#[test]
fn a_cut_of_ops_made_again_where_its_checkpoint_does_not_read_keeps_what_that_cut_would_have() {
    for torn in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let path = dir.path().join("log");
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        log.append("a", 3, b"left").unwrap();
        let left = log.anchor().unwrap().start;
        log.append_with("a", 3, b"dangling", &staged(1)).unwrap();
        log.append("a", 3, b"tx").unwrap();
        let kept = log.anchor().unwrap().start;
        log.append_with("a", 3, b"gone", &retiring(1)).unwrap();
        let gone = log.anchor().unwrap().start as usize;
        drop(log);
        // the op with which the workspace was gone is cut, and the cut
        // dies with its checkpoint written and not synced
        boot(2);
        let before = std::fs::read(&path).unwrap();
        let mut log = OpLog::open(dir.path()).unwrap();
        alt_oplog::v2::cut_dies_after(Some(2));
        let died = log.settle(but(&[b"gone"]));
        alt_oplog::v2::cut_dies_after(None);
        assert!(matches!(died, Err(OpLogError::Io(_))), "{died:?}");
        std::mem::forget(log);
        if torn {
            // the power goes with twenty bytes of the checkpoint not on
            // disk: what does not read there is cut as what it is, a
            // cut of ops that a sync can have ended among
            let mut bytes = std::fs::read(&path).unwrap();
            assert_ne!(bytes[gone + 20..gone + 40], before[gone + 20..gone + 40]);
            bytes[gone + 20..gone + 40].copy_from_slice(&before[gone + 20..gone + 40]);
            std::fs::write(&path, bytes).unwrap();
        }
        boot(3);
        let said = format!("Err(Kept {{ unheld: {left}, kept: {kept} }})");
        // a reader is refused as a writer is, before the cut is made
        // again
        let reader = OpLog::open_read_only(dir.path(), None).unwrap();
        let refused = reader.unconfirmed(but(&[b"left", b"dangling"]));
        assert_eq!(format!("{refused:?}"), said, "a reader, torn: {torn}");
        let mut log = OpLog::open(dir.path()).unwrap();
        log.refresh().unwrap();
        assert_eq!(log.len(), 5, "torn: {torn}");
        let refused = log.settle(but(&[b"left", b"dangling"]));
        assert_eq!(format!("{refused:?}"), said, "torn: {torn}");
    }
}
