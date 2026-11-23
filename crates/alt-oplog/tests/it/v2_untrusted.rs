//! Bytes of a version 2 log that are not what a writer laid: reading
//! them ends in the log as far as it reads or in an error, never in a
//! panic.

use std::fs;
use std::os::unix::fs::FileExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use alt_oplog::v2::frame::KIND_CHECKPOINT;
use alt_oplog::v2::receipt::{Outcome, Receipt};
use alt_oplog::{Anchor, Meta, Op, OpLog, OpLogError};

use super::counted::peak;
use super::v2_bounds::{stage, zero};
use super::v2_log::{boot, file_len, frames, grow, new_log};

/// A log with blocks of 1 KiB written in two boots: ops of several
/// workspaces, ref transactions, receipts, a frame longer than a block, a
/// cut, and frames left unsynced at its end. Handles opened after it run
/// in boot 2, which wrote the end of it.
pub fn varied(dir: &Path) {
    boot(1);
    alt_oplog::v2::create_with(dir, 10).unwrap();
    let mut log = OpLog::open(dir).unwrap();
    // op `n`: of a workspace or none, and every few a ref transaction
    // that a request made
    let of = |n: u8, requested: bool| Meta {
        workspace: (!n.is_multiple_of(3)).then_some([n % 3; 8]),
        undoable: n % 3 == 1,
        index_after: (n % 3 == 2).then_some([n; 20]),
        request: requested.then_some([n; 16]),
        linked: requested,
        ..Meta::default()
    };
    let receipt = |n: u8, first_op: u64| {
        let mut receipt = Receipt {
            request: [n; 16],
            binding: [2; 32],
            outcome: Outcome::Executed,
            exit: 0,
            first_op,
            ops: 1,
            finished_ms: 3,
            digest: [0; 32],
            len: 0,
            response: None,
        };
        receipt.answer(b"done\n");
        receipt
    };
    for n in 0..9u8 {
        log.append_with("a", n as u64, &[n; 40], &of(n, n % 4 == 0))
            .unwrap();
        if n % 4 == 0 {
            let first = log.anchor().unwrap().start;
            log.append_receipt(&receipt(n, first)).unwrap();
        }
        log.sync().unwrap();
    }
    log.append("a", 9, &vec![9; 2500]).unwrap();
    log.sync().unwrap();
    log.append("a", 10, b"gone").unwrap();
    drop(log);
    boot(2);
    let mut log = OpLog::open(dir).unwrap();
    let held = |op: &Op| Ok::<_, OpLogError>(op.payload != b"gone");
    assert!(log.settle(held).unwrap().is_some());
    for n in 11..17u8 {
        log.append_with("b", n as u64, &[n; 70], &of(n, false))
            .unwrap();
        log.sync().unwrap();
    }
    let _unsynced = alt_os::durability::leave_unsynced(true);
    for n in 17..30u8 {
        log.append_with("b", n as u64, &[n; 90], &of(n, n == 29))
            .unwrap();
    }
    let first = log.anchor().unwrap().start;
    log.append_receipt(&receipt(29, first)).unwrap();
    log.verify().unwrap();
    assert_eq!(log.state().unwrap().generation, 1);
}

/// The anchors of three ops of the log [`varied`] lays: its first, one
/// from before the cut, and one from after it.
pub fn anchors(dir: &Path) -> Vec<Anchor> {
    let log = OpLog::open(dir).unwrap();
    let anchor = |op: &Op| {
        let frame = op.frame.unwrap();
        Anchor {
            op: op.id,
            start: frame.at,
            end: frame.end,
            count: frame.seq,
        }
    };
    let ops = log.ops().unwrap();
    [0, 6, 13].map(|n| anchor(&ops[n])).to_vec()
}

/// Opens the log in `dir`, whatever its bytes are, in every way and
/// reads all of it: that ends in what the log holds or in an error. What
/// the reading costs stays within a multiple of how long the file is.
pub fn read_through(dir: &Path, anchors: &[Anchor], case: &str) {
    let len = file_len(dir);
    // a place that is not in the file is one that does not read, no failed read
    let no_io = |read: Result<(), &OpLogError>| assert!(!matches!(read, Err(OpLogError::Io(_))));
    let read = catch_unwind(AssertUnwindSafe(|| {
        for anchor in anchors {
            let (opened, held) = peak(|| OpLog::open_at(dir, Some(*anchor)));
            assert!(held as u64 <= 8 * len + 8192, "{held} bytes held");
            no_io(opened.as_ref().map(|_| ()));
            if let Ok(log) = opened {
                assert!(log.bytes_read() <= 8 * len, "{} read", log.bytes_read());
            }
        }
        for read_only in [true, false] {
            let (opened, held) = peak(|| match read_only {
                true => OpLog::open_read_only(dir, None),
                false => OpLog::open(dir),
            });
            no_io(opened.as_ref().map(|_| ()));
            let Ok(log) = opened else {
                assert!(held as u64 <= 4 * len + 8192, "{held} bytes held");
                continue;
            };
            let ((), held) = peak(|| {
                no_io(log.verify().as_ref().map(|_| ()));
                let ops = log.ops().map(<[Op]>::len);
                no_io(ops.as_ref().map(|_| ()));
                let head = log.head().map(|head| log.get(&head));
                assert!(ops.is_err() || !matches!(head, Some(Ok(None))));
            });
            assert!(held as u64 <= 8 * len + 8192, "{held} bytes held");
            assert!(log.bytes_read() <= 8 * len, "{} read", log.bytes_read());
        }
    }));
    if let Err(panic) = read {
        let said = panic.downcast_ref::<String>().map(String::as_str);
        let said = said.or(panic.downcast_ref::<&str>().copied());
        panic!("{case}: {}", said.unwrap_or("a panic"));
    }
}

#[test]
fn a_log_with_any_byte_changed_is_read_as_far_as_it_reads_or_refused() {
    let dir = alt_os::temp::tempdir().unwrap();
    varied(dir.path());
    let path = dir.path().join("log");
    let whole = fs::read(&path).unwrap();
    assert!(whole.len() > 8 * 1024);
    let anchors = anchors(dir.path());
    read_through(dir.path(), &anchors, "as it was written");
    // in the boot that wrote its end, and after the machine went down
    for (in_boot, change) in [(2, 0x01), (3, 0xff)] {
        boot(in_boot);
        for at in 0..whole.len() {
            let mut changed = whole.clone();
            changed[at] ^= change;
            fs::write(&path, &changed).unwrap();
            let case = format!("byte {at} in boot {in_boot}");
            read_through(dir.path(), &anchors, &case);
        }
    }
}

#[test]
fn an_open_that_reads_no_frame_after_the_checkpoint_it_went_back_to_ends_the_log_there() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    log.append("a", 1, &vec![7; 20_000]).unwrap();
    log.sync().unwrap();
    let synced = log.appended_len();
    let (all, _) = frames(dir.path());
    let last = all.last().unwrap();
    assert_eq!(
        (last.1, last.2),
        (synced, KIND_CHECKPOINT),
        "the boundary is where the checkpoint after the long frame ends"
    );
    stage(&mut log, 40);
    drop(log);
    assert!(file_len(dir.path()) > synced + 2 * 4096);
    let (all, _) = frames(dir.path());
    let first = all.iter().find(|frame| frame.0 == synced).unwrap();
    zero(dir.path(), first.0, first.1);
    boot(2);
    for read_only in [true, false] {
        let log = match read_only {
            true => OpLog::open_read_only(dir.path(), None).unwrap(),
            false => OpLog::open(dir.path()).unwrap(),
        };
        assert_eq!((log.len(), log.appended_len()), (1, synced));
        assert!(log.passed_over());
        let passed = log.passed_ops().unwrap();
        assert_eq!(passed.len(), 39, "every frame after the one that is gone");
        assert_eq!(passed.last().unwrap().payload, [39; 120]);
    }
}

#[test]
fn a_damaged_frame_after_the_checkpoint_an_open_went_back_to_is_damage_when_it_was_synced() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 60);
    let synced = log.appended_len();
    stage(&mut log, 40);
    drop(log);
    let (all, _) = frames(dir.path());
    // the first frame after the checkpoint of the block the boundary is in
    let block = synced / 4096 * 4096;
    let checkpoint = all.iter().find(|frame| frame.0 == block + 32).unwrap();
    let first = all.iter().find(|frame| frame.0 == checkpoint.1).unwrap();
    assert!(checkpoint.2 == KIND_CHECKPOINT && first.1 <= synced);
    let path = dir.path().join("log");
    let mut damaged = fs::read(&path).unwrap();
    damaged[first.0 as usize + 50] ^= 1;
    fs::write(&path, &damaged).unwrap();
    boot(2);
    for opened in [
        OpLog::open(dir.path()),
        OpLog::open_read_only(dir.path(), None),
    ] {
        assert!(matches!(
            opened,
            Err(OpLogError::Format("oplog record corrupt"))
        ));
    }
    assert_eq!(fs::read(&path).unwrap(), damaged);
}

/// A log with a hole of `hole` bytes after it, which begins as an op
/// frame with a body of `claimed` bytes would where that is given: what
/// opening it in two boots and looking for frames in the hole costs, in
/// bytes read and in reads. The next append cuts the hole off.
fn costs_with_a_hole(hole: u64, claimed: Option<u32>) -> Vec<(u64, u64)> {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 3);
    stage(&mut log, 20);
    drop(log);
    let len = file_len(dir.path());
    let file = fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("log"));
    let file = file.unwrap();
    file.set_len(len + hole).unwrap();
    if let Some(body) = claimed {
        let mut header = [0u8; 48];
        header[0] = 1;
        header[4..8].copy_from_slice(&body.to_le_bytes());
        file.write_all_at(&header, len).unwrap();
    }
    let mut costs = Vec::new();
    for in_boot in [1, 2] {
        boot(in_boot);
        let (log, held) = peak(|| OpLog::open(dir.path()).unwrap());
        assert!(held < 1 << 20, "{held} bytes held in boot {in_boot}");
        costs.push((log.bytes_read(), log.file_reads()));
        assert_eq!((log.len(), log.appended_len()), (23, len));
        assert!(log.passed_over());
        let (passed, held) = peak(|| log.passed().unwrap());
        assert_eq!(passed, (len, len + hole, None));
        assert!(held < 1 << 20, "{held} bytes held");
        costs.push((log.bytes_read(), log.file_reads()));
    }
    let mut log = OpLog::open(dir.path()).unwrap();
    let ((), held) = peak(|| {
        log.append("a", 1, b"next").unwrap();
    });
    assert!(held < 1 << 20, "{held} bytes held");
    costs.push((log.bytes_read(), log.file_reads()));
    assert_eq!(log.take_discarded()[0].unread, hole);
    assert!(file_len(dir.path()) < len + 4096);
    log.verify().unwrap();
    costs
}

#[test]
fn a_long_hole_after_the_log_is_passed_over_at_a_cost_its_length_does_not_raise() {
    // the file system tells where the hole is, and no block of it is read
    let costs = costs_with_a_hole(1 << 26, None);
    assert_eq!(costs_with_a_hole(1 << 30, None), costs);
    // a walk reads 128 KiB at once, also where the hole begins
    let most = costs.iter().map(|(bytes, _)| *bytes).max().unwrap();
    assert!(most < 1 << 20, "{costs:?}");

    // a hole that begins as a frame of 200 MiB would; and as frames
    // would whose last block holds a part of the eight bytes a frame
    // closes with, or little more than those: as long as one that fills
    // the block the log ends in, runs through 50,000 more and has so
    // many bytes in the one after
    let filling = {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 3);
        stage(&mut log, 20);
        4096 - (log.appended_len() % 4096) as u32
    };
    let parted = |last: u32| filling - 88 + 50_000 * 4064 + last;
    for claimed in [200 << 20, parted(2), parted(4), parted(6), parted(20)] {
        let costs = costs_with_a_hole(1 << 28, Some(claimed));
        let most = costs.iter().map(|(bytes, _)| *bytes).max().unwrap();
        assert!(most < 1 << 20, "a body of {claimed}: {costs:?}");
    }
}
