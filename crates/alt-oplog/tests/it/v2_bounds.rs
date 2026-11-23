//! How much of a version 2 log an open reads, what it takes on trust for
//! that, and what the frames say beside their ops.

use std::fs;
use std::path::Path;

use alt_oplog::v2::block::FileHeader;
use alt_oplog::v2::frame::{KIND_RECEIPT, Step};
use alt_oplog::v2::receipt::{Outcome, Receipt};
use alt_oplog::{Meta, Op, OpLog, OpLogError};

use super::v2_log::{boot, file_len, frames, grow, new_log};

/// Appends `count` ops of some 300 bytes and syncs none of them.
pub fn stage(log: &mut OpLog, count: u64) {
    let _unsynced = alt_os::durability::leave_unsynced(true);
    for n in 0..count {
        let meta = Meta {
            workspace: Some([1 + (n % 3) as u8; 8]),
            index_after: Some([n as u8; 20]),
            ..Meta::default()
        };
        log.append_with("human:root;user:root;verb:add", n, &[n as u8; 120], &meta)
            .unwrap();
    }
}

#[test]
fn ten_thousand_unsynced_frames_later_an_open_reads_no_more_than_a_block() {
    let dir = alt_os::temp::tempdir().unwrap();
    alt_oplog::v2::create(dir.path()).unwrap();
    let mut log = OpLog::open(dir.path()).unwrap();
    grow(&mut log, "a", 1);
    stage(&mut log, 10_000);
    let len = file_len(dir.path());
    assert!(len > 2_500_000);
    drop(log);
    for read_only in [true, false] {
        let log = match read_only {
            true => OpLog::open_read_only(dir.path(), None).unwrap(),
            false => OpLog::open(dir.path()).unwrap(),
        };
        // the file header and what the last block holds
        assert!(log.bytes_read() <= 64 + 16_384, "{}", log.bytes_read());
        assert_eq!(log.len(), 10_001);
        assert!(!log.unsettled() && !log.confirmed());
        assert_eq!(log.state().unwrap().workspaces.len(), 3);
    }
}

#[test]
fn after_the_machine_went_down_the_first_open_reads_through_what_was_left_and_the_next_a_block() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    let synced = log.appended_len();
    stage(&mut log, 500);
    drop(log);
    let len = file_len(dir.path());
    assert!(len > 30 * 4096);

    boot(2);
    let log = OpLog::open_read_only(dir.path(), None).unwrap();
    assert!(
        log.bytes_read() >= len - synced,
        "all that was left unsynced"
    );
    assert_eq!(log.len(), 502);
    assert!(log.unsettled());
    // until somebody has looked at it, every open reads through it
    let again = OpLog::open_read_only(dir.path(), None).unwrap();
    assert!(again.bytes_read() >= len - synced);
    let mut asked = 0;
    let unheld = log.unconfirmed_past(|_, left| {
        asked += 1;
        Ok::<_, OpLogError>(left)
    });
    assert_eq!((unheld.unwrap(), asked), (None, 500));

    for read_only in [true, false] {
        let log = match read_only {
            true => OpLog::open_read_only(dir.path(), None).unwrap(),
            false => OpLog::open(dir.path()).unwrap(),
        };
        assert!(log.bytes_read() <= 64 + 4096, "{}", log.bytes_read());
        assert_eq!(log.len(), 502);
        assert!(!log.unsettled());
    }
    // and another boot later, through it again
    boot(3);
    let log = OpLog::open(dir.path()).unwrap();
    assert!(log.bytes_read() >= len - synced && log.unsettled());
}

/// A log of some thirty blocks left unsynced past its second op, and
/// where its last block starts.
fn many_blocks(dir: &Path) -> u64 {
    let mut log = new_log(dir);
    grow(&mut log, "a", 2);
    stage(&mut log, 500);
    log.append("a", 1, b"the last one").unwrap();
    (file_len(dir) - 1) / 4096 * 4096
}

pub fn zero(dir: &Path, from: u64, to: u64) {
    let path = dir.join("log");
    let mut bytes = fs::read(&path).unwrap();
    bytes[from as usize..to as usize].fill(0);
    fs::write(path, bytes).unwrap();
}

fn payloads(ops: &[Op]) -> Vec<&[u8]> {
    ops.iter().map(|op| &op.payload[..]).collect()
}

#[test]
fn a_stretch_missing_from_what_this_boot_wrote_ends_the_log_before_it_and_is_told() {
    // a part of the last block after its checkpoint
    let dir = alt_os::temp::tempdir().unwrap();
    let last = many_blocks(dir.path());
    let len = file_len(dir.path());
    let whole = OpLog::open(dir.path()).unwrap().len();
    let (all, _) = frames(dir.path());
    let in_last: Vec<_> = all.iter().filter(|frame| frame.0 > last).collect();
    assert!(in_last.len() >= 4);
    let gone = in_last[1];
    zero(dir.path(), gone.0 + 10, gone.1 - 10);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.appended_len(), gone.0);
    assert!(log.passed_over() && log.len() < whole);
    assert_eq!(
        payloads(&log.passed_ops().unwrap()).last().unwrap(),
        b"the last one"
    );
    log.append("a", 2, b"next").unwrap();
    let cut = log.take_discarded();
    assert_eq!((cut[0].from, cut[0].to), (gone.0, len));
    assert_eq!(cut[0].ops.len(), in_last.len() - 2);
    assert_eq!(cut[0].unread, gone.1 - gone.0);
    log.verify().unwrap();

    // the header of the last block and its checkpoint: the checkpoint of
    // the block before is begun at, and the log ends with that block
    let dir = alt_os::temp::tempdir().unwrap();
    let last = many_blocks(dir.path());
    let (all, _) = frames(dir.path());
    let checkpoint = all.iter().find(|frame| frame.0 == last + 32).unwrap();
    let before = all.iter().rev().find(|frame| frame.1 <= last).unwrap();
    zero(dir.path(), last, checkpoint.1);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.appended_len(), before.1);
    assert!(log.passed_over());
    assert_eq!(
        payloads(&log.passed_ops().unwrap()).last().unwrap(),
        b"the last one"
    );
    log.append("a", 2, b"next").unwrap();
    assert_eq!(log.take_discarded()[0].from, before.1);
    log.verify().unwrap();

    // within what is synced it is damage, and nothing is cut
    let dir = alt_os::temp::tempdir().unwrap();
    let last = many_blocks(dir.path());
    OpLog::open(dir.path()).unwrap().bound_now().unwrap();
    let len = file_len(dir.path());
    for at in [len - 100, last + 40] {
        zero(dir.path(), at, at + 20);
        assert!(matches!(
            OpLog::open(dir.path()),
            Err(OpLogError::Format("oplog record corrupt"))
        ));
        assert_eq!(file_len(dir.path()), len);
    }

    // in a block before the last, which no open reads: the full check
    // finds it, as does whoever reads the history
    let dir = alt_os::temp::tempdir().unwrap();
    many_blocks(dir.path());
    zero(dir.path(), 10 * 4096 + 500, 10 * 4096 + 700);
    let log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), whole);
    assert!(log.verify().is_err() && log.ops().is_err());
}

#[test]
fn what_a_reader_would_not_vouch_for_stays_to_be_cut_whoever_appends_after_it() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    log.append("a", 3, b"gone").unwrap();
    drop(log);
    boot(2);
    let held = |op: &Op| Ok::<_, OpLogError>(op.payload != b"gone");
    let reader = OpLog::open_read_only(dir.path(), None).unwrap();
    assert_eq!(reader.unconfirmed(held).unwrap().unwrap().ops, 1);
    // a command that only stages appends without looking
    let mut staging = OpLog::open(dir.path()).unwrap();
    assert!(staging.unsettled());
    stage(&mut staging, 40);
    drop(staging);
    assert!(file_len(dir.path()) > 2 * 4096);

    let mut writer = OpLog::open(dir.path()).unwrap();
    assert!(writer.unsettled());
    let cut = writer.settle(held).unwrap().unwrap();
    assert_eq!(cut.ops, 41, "the op and all that was appended after it");
    assert_eq!(writer.len(), 2);
    assert_eq!(payloads(&writer.take_discarded()[0].ops)[0], b"gone");
    writer.verify().unwrap();
}

#[test]
fn a_log_with_a_feature_this_build_does_not_know_is_read_or_refused_as_the_feature_says() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    drop(log);
    let path = dir.path().join("log");
    let header = FileHeader::decode(&fs::read(&path).unwrap()).unwrap();
    let with = |header: FileHeader| {
        let mut bytes = fs::read(&path).unwrap();
        bytes[..64].copy_from_slice(&header.encode());
        fs::write(&path, bytes).unwrap();
    };
    with(FileHeader {
        compatible: 1,
        ..header
    });
    OpLog::open(dir.path())
        .unwrap()
        .append("a", 1, b"x")
        .unwrap();

    let written = fs::read(&path).unwrap();
    with(FileHeader {
        read_compatible: 1,
        ..header
    });
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 3);
    let said = "the oplog uses a feature this alt does not know to write";
    assert!(matches!(log.append("a", 1, b"x"), Err(OpLogError::Format(s)) if s == said));
    assert_eq!(fs::read(&path).unwrap()[64..], written[64..]);

    with(FileHeader {
        incompatible: 1 | 4,
        ..header
    });
    let said = "the oplog uses a feature this alt does not know";
    for opened in [
        OpLog::open(dir.path()),
        OpLog::open_read_only(dir.path(), None),
    ] {
        assert!(matches!(opened, Err(OpLogError::Format(s)) if s == said));
    }
}

#[test]
fn a_frame_longer_than_a_block_is_read_past_by_the_next_open() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 3);
    let long = log.append("a", 1, &vec![7; 200_000]).unwrap();
    log.sync().unwrap();
    log.append("a", 2, b"after").unwrap();
    log.sync().unwrap();
    drop(log);
    let mut log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.len(), 5);
    assert!(log.bytes_read() <= 64 + 4096);
    assert_eq!(payloads(log.recent()), [b"after"]);
    assert_eq!(log.get(&long).unwrap().unwrap().payload.len(), 200_000);
    log.append("a", 3, b"more").unwrap();
    log.verify().unwrap();
}

#[test]
fn what_frames_say_of_their_ops_is_what_a_checkpoint_holds_of_each_workspace() {
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = new_log(dir.path());
    let of = |workspace: u8| Meta {
        workspace: Some([workspace; 8]),
        ..Meta::default()
    };
    let mut at = Vec::new();
    let append = |log: &mut OpLog, at: &mut Vec<u64>, meta: Meta| {
        log.append_with("a", 1, b"op", &meta).unwrap();
        at.push(log.anchor().unwrap().start);
    };
    append(
        &mut log,
        &mut at,
        Meta {
            undoable: true,
            index_after: Some([1; 20]),
            ..of(1)
        },
    );
    append(
        &mut log,
        &mut at,
        Meta {
            step: Some(Step::Begin),
            ..of(2)
        },
    );
    append(
        &mut log,
        &mut at,
        Meta {
            linked: true,
            undoable: true,
            ..of(1)
        },
    );
    let (begun, undone) = (at[1], at[2]);
    append(
        &mut log,
        &mut at,
        Meta {
            step: Some(Step::Within(begun)),
            ..of(2)
        },
    );
    append(
        &mut log,
        &mut at,
        Meta {
            request: Some([5; 16]),
            inverse_of: Some(undone),
            ..of(1)
        },
    );
    append(&mut log, &mut at, of(3));
    let mut receipt = Receipt {
        request: [5; 16],
        binding: [6; 32],
        outcome: Outcome::Executed,
        exit: 0,
        first_op: at[4],
        ops: 1,
        finished_ms: 7,
        digest: [0; 32],
        len: 0,
        response: None,
    };
    receipt.answer(b"staged 1 path(s)\n");
    log.append_receipt(&receipt).unwrap();
    log.sync().unwrap();
    for reopened in [false, true] {
        if reopened {
            // enough to begin the next block with a checkpoint
            for n in 0..20 {
                log.append("b", n, &[0; 150]).unwrap();
            }
            log = OpLog::open(dir.path()).unwrap();
            assert!(log.recent().len() < 20);
        }
        let state = log.state().unwrap();
        let one = state.workspace(&[1; 8]).unwrap();
        assert_eq!(
            (one.undoable, one.index_at, one.index_sum),
            (at[2], at[0], [1; 20])
        );
        assert_eq!(one.step, 0);
        let two = state.workspace(&[2; 8]).unwrap();
        assert_eq!((two.last, two.undoable, two.step), (at[3], 0, at[1]));
        if !reopened {
            assert_eq!((one.last, state.linked_at, state.linked), (at[4], at[2], 1));
            assert_eq!(state.workspaces.len(), 3);
            let last = log.recent()[4].frame.unwrap();
            assert_eq!(
                (last.request, last.inverse_of),
                (Some([5; 16]), Some(at[2]))
            );
        }
    }
    // a step ends, a workspace goes
    log.append_with(
        "a",
        2,
        b"op",
        &Meta {
            step: Some(Step::Close),
            ..of(2)
        },
    )
    .unwrap();
    log.append_with(
        "a",
        2,
        b"op",
        &Meta {
            retired: true,
            ..of(3)
        },
    )
    .unwrap();
    let state = log.state().unwrap();
    assert_eq!(state.workspace(&[2; 8]).unwrap().step, 0);
    assert!(state.workspace(&[3; 8]).is_none());
    assert_eq!(
        frames(dir.path())
            .0
            .iter()
            .filter(|frame| frame.2 == KIND_RECEIPT)
            .count(),
        1
    );
    log.verify().unwrap();
}
