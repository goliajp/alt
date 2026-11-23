//! What the full check of a version 2 log holds it to beyond what every
//! read does, and what a build appends after.

use std::fs;
use std::path::Path;

use alt_oplog::v2::block::{BlockHeader, FileHeader};
use alt_oplog::v2::compose::{Composer, New};
use alt_oplog::v2::frame::{KIND_OP, KIND_RECEIPT, REQUIRED, SKIPPABLE};
use alt_oplog::v2::receipt::Receipt;
use alt_oplog::v2::scan::{Stop, Walk};
use alt_oplog::v2::state::State;
use alt_oplog::{Meta, Op, OpId, OpLog, OpLogError};

use super::v2_fields::{Laid, laid, written};
use super::v2_image::{Image, walked};
use super::v2_log::{boot, grow, new_log};
use super::v2_untrusted::varied;

fn said(checked: Result<(), OpLogError>) -> &'static str {
    match checked {
        Err(OpLogError::Format(said)) => said,
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_full_check_holds_a_block_header_to_the_checkpoint_before_it_and_its_frame() {
    let mut image = Image::new(10);
    image.op(1, b"first");
    image.op(1, &vec![5; 2600]);
    for n in 0..20u8 {
        image.op(2, &[n; 50]);
    }
    let layout = image.layout();
    walked(layout, &image.bytes, true).unwrap();
    let mut inside = 0;
    for at in (1024..image.bytes.len()).step_by(1024) {
        let header = layout.decode_header(&image.bytes[at..], at as u64).unwrap();
        // one a frame runs across, or one a block opens with
        inside += (header.first != 32) as usize;
        let of_another_generation = BlockHeader {
            generation: header.generation + 1,
            ..header
        };
        let names_another_checkpoint = BlockHeader {
            checkpoint: header.checkpoint + 8,
            ..header
        };
        for (changed, why) in [
            (
                of_another_generation,
                "an oplog block is of another generation than the frame in it",
            ),
            (
                names_another_checkpoint,
                "an oplog block names another checkpoint than the one before it",
            ),
        ] {
            let mut other = image.bytes.clone();
            other[at..at + 32].copy_from_slice(&layout.encode_header(&changed));
            // every read takes the frames as they are
            let (frames, state, stop) = walked(layout, &other, false).unwrap();
            assert_eq!((&frames, &state), (&image.frames, &image.state));
            assert_eq!(stop, Stop::End);
            assert_eq!(said(walked(layout, &other, true).map(|_| ())), why);
        }
    }
    assert!(inside >= 2, "the long frame runs across block headers");
}

/// The log `whole` with the last frame `pick` chooses changed by
/// `change`, its checksum holding.
fn rewritten(whole: &[u8], pick: impl Fn(&Laid) -> bool, change: impl Fn(&mut Laid)) -> Vec<u8> {
    let (layout, frames) = laid(whole);
    let frame = frames.iter().rev().find(|frame| pick(frame)).unwrap();
    let mut other = Laid::of(frame);
    change(&mut other);
    let bytes = other.with(&layout, &other.head, &other.body).unwrap();
    let mut changed = whole.to_vec();
    changed[frame.at as usize..frame.end as usize].copy_from_slice(&bytes);
    changed
}

#[test]
fn the_full_check_holds_a_receipt_to_what_it_answered_and_to_the_frames_of_its_request() {
    let dir = alt_os::temp::tempdir().unwrap();
    varied(dir.path());
    let (whole, _) = written(dir.path());
    let checked = |bytes: &[u8]| {
        fs::write(dir.path().join("log"), bytes).unwrap();
        let log = OpLog::open(dir.path()).unwrap();
        // every read takes the receipt as it is
        assert_eq!(log.ops().unwrap().len(), 29);
        log.verify().map(|_| ())
    };
    checked(&whole).unwrap();
    // the receipt of the request that made op 4, within what is synced
    let of_four =
        |frame: &Laid| frame.head.kind == KIND_RECEIPT && frame.ext.request == Some([4; 16]);
    let with = |change: &dyn Fn(&mut Receipt)| {
        rewritten(&whole, of_four, |frame| {
            let mut receipt = Receipt::decode(&frame.body).unwrap();
            change(&mut receipt);
            frame.body = receipt.encode();
        })
    };
    let disagrees = "an oplog receipt disagrees with the frames of its request";
    assert_eq!(said(checked(&with(&|receipt| receipt.ops = 2))), disagrees);
    assert_eq!(
        said(checked(&with(&|receipt| receipt.first_op += 1))),
        disagrees
    );
    let unread = "an oplog receipt does not read";
    assert_eq!(
        said(checked(&with(&|receipt| receipt.digest[0] ^= 1))),
        unread
    );
    let of_another = rewritten(&whole, of_four, |frame| frame.ext.request = Some([7; 16]));
    assert_eq!(said(checked(&of_another)), unread);
    let cut_short = rewritten(&whole, of_four, |frame| frame.body[0] = 9);
    assert_eq!(said(checked(&cut_short)), unread);
}

/// Appends to the log in `dir`, as an alt that knows more kinds of frames
/// would in boot 1, a frame of a kind this build does not know with
/// `flags`, and then `ops` ops.
pub fn appended_by_a_later_alt(dir: &Path, flags: u8, ops: usize) {
    let mut bytes = fs::read(dir.join("log")).unwrap();
    let layout = FileHeader::decode(&bytes).unwrap().layout();
    let mut state = State::empty();
    let walk = Walk {
        layout,
        data: &bytes,
        base: 0,
        strict: false,
    };
    walk.run(&mut state, &mut |_, _, _| Ok(())).unwrap();
    let composer = Composer {
        layout,
        boot: [1; 16],
        synced: state.synced,
    };
    let unknown = New {
        kind: 9,
        flags,
        body: b"from a later alt",
        ..New::default()
    };
    bytes.extend(composer.frame(&mut state, &unknown).unwrap());
    for n in 0..ops {
        let body = alt_oplog::v2::op_body(&state.head, n as u64, "later", &[n as u8; 150]);
        let op = New {
            kind: KIND_OP,
            body: &body,
            op: Some(OpId(*alt_hash::blake3::hash(&body).as_bytes())),
            ..New::default()
        };
        bytes.extend(composer.frame(&mut state, &op).unwrap());
    }
    fs::write(dir.join("log"), bytes).unwrap();
}

#[test]
fn nothing_is_appended_after_a_frame_that_says_it_must_be_known_and_is_not() {
    let must_know = "the oplog holds a frame this alt must know to append after it";
    for (flags, later_ops) in [
        (SKIPPABLE, 0),
        (SKIPPABLE | REQUIRED, 0),
        (SKIPPABLE | REQUIRED, 60),
    ] {
        let dir = alt_os::temp::tempdir().unwrap();
        boot(1);
        let mut log = new_log(dir.path());
        grow(&mut log, "a", 2);
        drop(log);
        appended_by_a_later_alt(dir.path(), flags, later_ops);
        let before = fs::read(dir.path().join("log")).unwrap();
        let mut log = OpLog::open(dir.path()).unwrap();
        assert_eq!(log.len(), 2 + later_ops);
        // over blocks, the handle begins at a checkpoint after the frame
        assert!(later_ops == 0 || log.recent().len() < 20);
        assert_eq!(log.verify().unwrap(), 1, "a frame of a kind it skips");
        let appended = log.append("a", 3, b"after it").map(|_| ());
        if flags & REQUIRED == 0 {
            appended.unwrap();
            continue;
        }
        assert_eq!(said(appended), must_know);
        if later_ops != 0 {
            // nor is the log cut after it, which a checkpoint would say there
            boot(2);
            let mut log = OpLog::open(dir.path()).unwrap();
            let cut = log.settle(|_: &Op| Ok::<_, OpLogError>(false));
            assert_eq!(said(cut.map(|_| ())), must_know);
        }
        assert_eq!(fs::read(dir.path().join("log")).unwrap(), before);
    }
    // an op that says so is of a kind this layer knows: who reads the ops decides
    let dir = alt_os::temp::tempdir().unwrap();
    let mut log = new_log(dir.path());
    let required = Meta {
        required: true,
        ..Meta::default()
    };
    log.append_with("a", 1, b"of a kind to be known", &required)
        .unwrap();
    let at = log.anchor().unwrap().start;
    log.append("a", 2, b"after it").unwrap();
    assert_eq!(log.state().unwrap().required, at);
    assert_eq!(
        OpLog::open(dir.path()).unwrap().state().unwrap().required,
        at
    );
}

#[test]
fn a_sync_records_no_more_than_the_handle_wrote_before_a_frame_it_would_have_to_know() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    grow(&mut log, "a", 2);
    let synced = log.appended_len();
    log.append("a", 3, b"its own").unwrap();
    let own = log.appended_len();
    // a later alt appends meanwhile, and the handle reads it
    appended_by_a_later_alt(dir.path(), SKIPPABLE | REQUIRED, 3);
    log.refresh().unwrap();
    assert!(log.len() == 6 && log.appended_len() > own);
    assert_eq!(log.bounded_to(), Some(synced));
    log.sync().unwrap();
    assert_eq!(alt_oplog::bounded_in(dir.path()), Some(own));
    // there is nothing more of its own to sync
    let noted = fs::read(dir.path().join("tail")).unwrap();
    log.sync().unwrap();
    assert_eq!(fs::read(dir.path().join("tail")).unwrap(), noted);
    log.verify().unwrap();
}

#[test]
fn an_op_from_before_a_cut_is_looked_up_under_the_generation_it_was_written_in() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    let anchors = grow(&mut log, "a", 30);
    log.append("a", 99, b"gone").unwrap();
    let cut_at = log.anchor().unwrap().start;
    drop(log);
    // the log is cut in its second block, where ops before the cut lie
    let before = anchors.iter().rev().find(|anchor| anchor.start / 4096 == 1);
    let before = *before.unwrap();
    assert_eq!((cut_at / 4096, anchors[2].start / 4096), (1, 0));
    boot(2);
    let mut log = OpLog::open(dir.path()).unwrap();
    let held = |op: &Op| Ok::<_, OpLogError>(op.payload != b"gone");
    assert!(log.settle(held).unwrap().is_some());
    grow(&mut log, "b", 60);
    assert_eq!(log.state().unwrap().generation, 1);
    drop(log);
    for anchor in [anchors[2], before] {
        let log = OpLog::open_at(dir.path(), Some(anchor)).unwrap();
        assert!(log.anchored(), "the anchor at {}", anchor.start);
        assert!(log.recent().len() < 30);
        // the frame of the anchor, and for one the block it is in
        assert!(log.bytes_read() < 3 * 4096, "{}", log.bytes_read());
    }
}
