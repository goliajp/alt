//! Version 2 logs as a reader walks them: what is cut short or damaged
//! ends the log at the frame before it, frames lie in blocks as the format
//! says, and the checkpoint found from the end of the file stands for the
//! frames before it.

use alt_oplog::v2::block::{BLOCK_HEADER_LEN, BlockHeader, FILE_HEADER_LEN};
use alt_oplog::v2::checkpoint::Checkpoint;
use alt_oplog::v2::frame::{
    Cut, Ext, Head, KIND_CHECKPOINT, KIND_OP, SKIPPABLE, Seed, Step, encode, parse,
};
use alt_oplog::v2::scan::{Stop, locate};
use alt_oplog::v2::state::State;
use alt_oplog::{OpId, OpLogError};

use super::v2_image::{Image, walked};

/// A log of 1 KiB blocks that holds ops of two workspaces, ref
/// transactions and receipts across several blocks.
fn several_blocks() -> Image {
    let mut image = Image::new(10);
    for n in 0..40u8 {
        let ext = Ext {
            index_after: (n % 5 == 0).then_some([n; 20]),
            step: [Some(Step::Begin), None, Some(Step::Close)][n as usize % 3],
            ..Ext::default()
        };
        image.op_with(n % 3, &vec![n; 10 + (n as usize * 7) % 90], ext, n % 4 == 0);
        if n % 2 == 0 {
            image.receipt(&[n; 30]);
        }
    }
    assert!(image.bytes.len() > 6 << 10);
    image
}

#[test]
fn a_log_cut_short_anywhere_ends_at_the_last_frame_that_is_whole() {
    let image = several_blocks();
    let layout = image.layout();
    for len in FILE_HEADER_LEN..=image.bytes.len() {
        let (frames, state, stop) = walked(layout, &image.bytes[..len], false).unwrap();
        let whole = image.frames.iter().filter(|frame| frame.1 <= len as u64);
        assert_eq!(frames, whole.copied().collect::<Vec<_>>(), "cut at {len}");
        let end = frames
            .last()
            .map_or(FILE_HEADER_LEN as u64, |frame| frame.1);
        assert_eq!(state.end, end);
        assert_eq!(stop == Stop::End, end == len as u64, "cut at {len}");
    }
}

#[test]
fn a_log_with_any_bit_changed_ends_at_the_frame_before_it() {
    let image = several_blocks();
    let layout = image.layout();
    for at in FILE_HEADER_LEN..image.bytes.len() {
        for bit in [0, 3, 7] {
            let mut damaged = image.bytes.clone();
            damaged[at] ^= 1 << bit;
            let (frames, _, stop) = walked(layout, &damaged, false).unwrap();
            let before = image.frames.iter().filter(|frame| frame.1 <= at as u64);
            assert_eq!(
                frames,
                before.copied().collect::<Vec<_>>(),
                "byte {at} bit {bit}"
            );
            assert_eq!(stop, Stop::Unread);
        }
    }
}

#[test]
fn a_frame_fills_a_block_to_its_last_byte_or_begins_the_next_one() {
    for short in [0u64, 1, 2, 100] {
        let mut image = Image::new(10);
        image.op(1, b"first");
        // an op that leaves `short` bytes of the first block
        let room = 1024 - image.state.end - short;
        let payload = (room - Image::op_len(0)) as usize;
        image.op(1, &vec![1; payload]);
        assert_eq!(image.state.end, 1024 - short);
        assert_eq!(image.frames.len(), 2, "no checkpoint yet, {short} short");
        // the next op does not fit: zeros to the end of the block, a block
        // header, a checkpoint, the op
        image.op(2, b"next");
        let [.., (checkpoint, _, kind), (op, end, _)] = image.frames[..] else {
            panic!()
        };
        assert_eq!((checkpoint, kind), (1024 + 32, KIND_CHECKPOINT));
        assert!(
            image.bytes[(1024 - short) as usize..1024]
                .iter()
                .all(|b| *b == 0)
        );
        assert_eq!(&image.bytes[1024..1028], b"ALTB");
        assert_eq!(image.state.checkpoint, checkpoint);
        assert_eq!(end, image.bytes.len() as u64);
        assert!(op > checkpoint && end < 2048);
        let (frames, state, stop) = walked(image.layout(), &image.bytes, true).unwrap();
        assert_eq!(
            (frames, state, stop),
            (image.frames, image.state, Stop::End)
        );
    }
}

#[test]
fn a_frame_longer_than_a_block_runs_across_block_headers_and_a_checkpoint_follows_it() {
    // lengths around what makes the frame end at the end of a block
    for payload in (2500..2500 + 1024).step_by(31).chain([1024 * 9, 1 << 17]) {
        let mut image = Image::new(10);
        image.op(1, b"first");
        let long = image.op(1, &vec![5; payload]);
        image.op(2, b"after");
        let kinds: Vec<u8> = image.frames.iter().map(|frame| frame.2).collect();
        // the long op begins a block after that block's checkpoint
        assert_eq!(kinds[..3], [KIND_OP, KIND_CHECKPOINT, KIND_OP]);
        assert_eq!(kinds[3], KIND_CHECKPOINT, "one right after the long op");
        let (at, end, _) = image.frames[2];
        assert!(at > 1024 + 32 && end > at + payload as u64);
        let headers = (end - 1) / 1024 - at / 1024;
        assert_eq!(
            end - at,
            Image::op_len(payload) + headers * BLOCK_HEADER_LEN as u64
        );
        let (frames, state, stop) = walked(image.layout(), &image.bytes, true).unwrap();
        assert_eq!(
            (frames, &state, stop),
            (image.frames.clone(), &image.state, Stop::End)
        );
        assert_eq!(state.workspace(&[1; 8]).unwrap().last, at);
        assert_eq!(state.seq, 3);
        assert_ne!(state.head, long);

        // a block header inside it that says the frame ends elsewhere is not
        // one of this frame's: the log ends before the frame
        let inner = (at / 1024 + 1) * 1024;
        let said = image
            .layout()
            .decode_header(&image.bytes[inner as usize..], inner);
        let mut other = image.bytes.clone();
        let moved = BlockHeader {
            first: said.unwrap().first ^ 32,
            ..said.unwrap()
        };
        other[inner as usize..][..32].copy_from_slice(&image.layout().encode_header(&moved));
        let (frames, _, stop) = walked(image.layout(), &other, false).unwrap();
        assert_eq!((frames.len(), stop), (2, Stop::Unread));

        // from the end of the file a reader begins at the checkpoint after it
        let len = image.bytes.len() as u64;
        let mut read = |at: u64, len: usize| Ok(image.bytes[at as usize..][..len].to_vec());
        let began = locate(&image.layout(), len, &mut read).unwrap();
        assert!(began.state.checkpoint >= end, "payload {payload}");
    }
}

#[test]
fn the_checkpoint_found_from_the_end_of_a_file_of_any_length_is_the_last_one_a_scan_meets() {
    let mut image = several_blocks();
    image.op(1, &vec![3; 3000]);
    image.op(2, b"after the long one");
    let layout = image.layout();
    for len in FILE_HEADER_LEN..=image.bytes.len() {
        let data = &image.bytes[..len];
        let (frames, state, _) = walked(layout, data, false).unwrap();
        let last = frames.iter().rev().find(|frame| frame.2 == KIND_CHECKPOINT);
        let mut read = |at: u64, len: usize| Ok(data[at as usize..][..len].to_vec());
        let mut began = locate(&layout, len as u64, &mut read).unwrap().state;
        assert_eq!(
            began.checkpoint,
            last.map_or(0, |frame| frame.0),
            "length {len}"
        );
        assert_eq!(began.end, last.map_or(64, |frame| frame.1), "length {len}");
        // and reading on from there comes to what reading all of it comes to
        let walk = alt_oplog::v2::scan::Walk {
            layout,
            data,
            base: 0,
            strict: false,
        };
        walk.run(&mut began, &mut |_, _, _| Ok(())).unwrap();
        assert_eq!(began, state, "length {len}");
    }
}

#[test]
fn frames_written_before_a_cut_do_not_read_where_others_were_appended_after_it() {
    let mut image = Image::new(10);
    image.op(1, b"kept");
    let kept = (image.state.clone(), image.bytes.len());
    image.op(1, b"cut");
    image.op(2, b"cut too");
    let cut_off = image.bytes[kept.1..].to_vec();

    // a writer cuts both, says so in a checkpoint under the next
    // generation, and appends other ops
    image.state = kept.0;
    image.bytes.truncate(kept.1);
    image.frames.truncate(1);
    image.state.generation += 1;
    let cut = Cut {
        bytes: cut_off.len() as u64,
        ops: 2,
    };
    let from = image.state.clone();
    let added = image
        .composer
        .checkpoint(&mut image.state, Some(cut))
        .unwrap();
    image.bytes.extend_from_slice(&added);
    image.op(1, b"anew");
    let (frames, state, stop) = walked(image.layout(), &image.bytes, true).unwrap();
    assert_eq!((state.generation, state.seq, stop), (1, 2, Stop::End));
    assert_eq!(
        frames[1],
        (from.end, from.end + added.len() as u64, KIND_CHECKPOINT)
    );

    // the old bytes back in their place end the log where it was cut
    let mut old = image.bytes[..kept.1].to_vec();
    old.extend_from_slice(&added);
    old.extend_from_slice(&cut_off);
    let (frames, _, stop) = walked(image.layout(), &old, false).unwrap();
    assert_eq!((frames.len(), stop), (2, Stop::Unread));
}

/// The log of `image` with the frame at `at` written anew by `change`.
fn rewritten(
    image: &Image,
    at: u64,
    change: impl Fn(&mut Head, &mut Vec<u8>, &mut Option<OpId>),
) -> Vec<u8> {
    let (_, end, _) = *image.frames.iter().find(|frame| frame.0 == at).unwrap();
    let seed = Seed::new(&image.layout().id, 0);
    let view = parse(&seed, &image.bytes[at as usize..end as usize]).unwrap();
    let (mut head, mut body, mut op) = (view.head, view.body.to_vec(), view.op);
    change(&mut head, &mut body, &mut op);
    let frame = encode(&seed, &head, &view.ext, &body, op.as_ref()).unwrap();
    assert_eq!(frame.len() as u64, end - at);
    let mut bytes = image.bytes.clone();
    bytes[at as usize..end as usize].copy_from_slice(&frame);
    bytes
}

#[test]
fn a_frame_whose_checksum_holds_is_still_held_to_the_frames_before_it() {
    let mut image = Image::new(10);
    for n in 0..30u8 {
        image.op(n % 2 + 1, &[n; 10]);
    }
    let layout = image.layout();
    let op = image.frames[4].0;
    let stops_before = |bytes: &[u8]| {
        let (frames, _, stop) = walked(layout, bytes, false).unwrap();
        assert_eq!((frames.len(), stop), (4, Stop::Unread));
    };
    // another frame before it, another count of ops, a claim past itself
    stops_before(&rewritten(&image, op, |head, _, _| head.prev += 1));
    stops_before(&rewritten(&image, op, |head, _, _| head.seq += 1));
    stops_before(&rewritten(&image, op, |head, _, _| head.synced = op + 1));
    stops_before(&rewritten(&image, op, |head, _, _| {
        head.prev_in_workspace = 0
    }));

    // a parent that is not the op before it breaks the chain for every reader
    let broken = rewritten(&image, op, |_, body, id| {
        body[0] ^= 1;
        *id = Some(OpId(*alt_hash::blake3::hash(body).as_bytes()));
    });
    for strict in [false, true] {
        assert!(matches!(
            walked(layout, &broken, strict),
            Err(OpLogError::Format("oplog chain broken"))
        ));
    }
    // an id that is not the hash of the body is found by the full check alone
    let renamed = rewritten(&image, op, |_, _, id| id.as_mut().unwrap().0[31] ^= 1);
    assert!(matches!(
        walked(layout, &renamed, false),
        Err(OpLogError::Format("oplog chain broken"))
    ));
    let last = image.frames.last().unwrap().0;
    let renamed = rewritten(&image, last, |_, _, id| id.as_mut().unwrap().0[31] ^= 1);
    assert_eq!(walked(layout, &renamed, false).unwrap().2, Stop::End);
    assert!(matches!(
        walked(layout, &renamed, true),
        Err(OpLogError::Format("an op's id is not the hash of its body"))
    ));
    // so is a checkpoint that says something the frames before it do not
    let (checkpoint, ..) = *image
        .frames
        .iter()
        .find(|frame| frame.2 == KIND_CHECKPOINT)
        .unwrap();
    let off = rewritten(&image, checkpoint, |_, body, _| body[21] ^= 1);
    assert_eq!(walked(layout, &off, false).unwrap().2, Stop::End);
    assert!(matches!(
        walked(layout, &off, true),
        Err(OpLogError::Format(
            "an oplog checkpoint disagrees with the frames before it"
        ))
    ));
}

#[test]
fn a_frame_of_an_unknown_kind_is_skipped_only_when_it_says_it_may_be() {
    for (flags, read) in [(SKIPPABLE, true), (0, false)] {
        let mut image = Image::new(10);
        image.op(1, b"before");
        let from = image.state.clone();
        let new = alt_oplog::v2::compose::New {
            kind: 9,
            flags,
            body: b"from a later alt",
            ..Default::default()
        };
        let added = image.composer.frame(&mut image.state, &new).unwrap();
        image.bytes.extend_from_slice(&added);
        let body = alt_oplog::v2::op_body(&image.state.head, 1, "actor", b"after");
        let id = OpId(*alt_hash::blake3::hash(&body).as_bytes());
        let after = alt_oplog::v2::compose::New {
            kind: KIND_OP,
            body: &body,
            op: Some(id),
            ..Default::default()
        };
        let added = image.composer.frame(&mut image.state, &after).unwrap();
        image.bytes.extend_from_slice(&added);
        match walked(image.layout(), &image.bytes, false) {
            Ok((frames, state, Stop::End)) if read => {
                assert_eq!((frames.len(), state.seq, state.head), (3, 2, id));
                assert_eq!(frames[1].0, from.end);
            }
            Err(OpLogError::Format(said)) if !read => {
                assert_eq!(
                    said,
                    "the oplog holds a frame of a kind this alt does not know"
                )
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn the_state_a_checkpoint_gives_is_the_state_before_it() {
    let image = several_blocks();
    let seed = Seed::new(&image.layout().id, 0);
    let mut checked = 0;
    for (at, end, kind) in image.frames.iter().copied() {
        if kind != KIND_CHECKPOINT {
            continue;
        }
        let bytes = &image.bytes[at as usize..end as usize];
        let said = Checkpoint::decode(parse(&seed, bytes).unwrap().body).unwrap();
        // the frames before it, and then the checkpoint frame itself
        let (_, mut expected, _) =
            walked(image.layout(), &image.bytes[..at as usize], false).unwrap();
        (expected.last, expected.end, expected.checkpoint) = (at, end, at);
        // it holds the whole table, and nothing changed since
        assert_eq!(said.base, 0);
        (expected.base, expected.changed) = (at, Vec::new());
        assert_eq!(State::of(said, Vec::new(), at, end), expected);
        checked += 1;
    }
    assert!(checked >= 6);
}
