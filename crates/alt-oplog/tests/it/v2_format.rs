//! The pieces of the version 2 file format as bytes: what is encoded
//! reads back, and what is not what was encoded does not.

use alt_oplog::v2::block::{BlockHeader, FILE_HEADER_LEN, FileHeader};
use alt_oplog::v2::checkpoint::{Checkpoint, Workspace};
use alt_oplog::v2::frame::{
    Cut, Ext, Head, KIND_CHECKPOINT, KIND_OP, KIND_RECEIPT, OdbBounds, Seed, Step, encode, parse,
};
use alt_oplog::v2::receipt::{INLINE_MAX, Outcome, Receipt};
use alt_oplog::{OpId, OpLogError};

use super::v2_image::header;

fn full_ext() -> Ext {
    Ext {
        odb: Some(OdbBounds {
            lens: [1, 2, 3, u64::MAX],
            generation: 9,
        }),
        inverse_of: Some(77),
        request: Some([3; 16]),
        linked: Some(0),
        cut: Some(Cut {
            bytes: 5,
            ops: 2,
            after: 640,
            generation: 3,
        }),
        index_after: Some([8; 20]),
        step: Some(Step::Within(640)),
        retired: true,
    }
}

#[test]
fn every_kind_of_frame_reads_back_as_it_was_written() {
    let seed = Seed::new(&[1; 16], 3);
    let head = |kind| Head {
        kind,
        flags: 0b110,
        prev: 64,
        prev_in_workspace: 10,
        synced: 20,
        workspace: [4; 8],
        seq: 6,
    };
    let id = OpId([9; 32]);
    for (kind, ext, body, op) in [
        (KIND_OP, full_ext(), &b"an op's body"[..], Some(&id)),
        (KIND_OP, Ext::default(), b"", Some(&id)),
        (KIND_CHECKPOINT, Ext::default(), b"checkpoint", None),
        (KIND_RECEIPT, full_ext(), b"receipt", None),
    ] {
        let bytes = encode(&seed, &head(kind), &ext, body, op).unwrap();
        let view = parse(&seed, &bytes).unwrap();
        assert_eq!((view.head, &view.ext, view.body), (head(kind), &ext, body));
        assert_eq!(view.op, op.copied());
        // another log, another generation
        assert!(parse(&Seed::new(&[2; 16], 3), &bytes).is_none());
        assert!(parse(&Seed::new(&[1; 16], 4), &bytes).is_none());
        // a byte more or less is another frame
        assert!(parse(&seed, &bytes[..bytes.len() - 1]).is_none());
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(parse(&seed, &longer).is_none());
    }
    for step in [Step::Begin, Step::Within(1 << 40), Step::Close] {
        let ext = Ext {
            step: Some(step),
            ..Ext::default()
        };
        assert_eq!(Ext::decode(&ext.encode()), Some(ext));
    }
    // an item nobody knows is passed over; a known one of another length is not
    assert_eq!(Ext::decode(&[200, 2, 1, 1]), Some(Ext::default()));
    assert_eq!(Ext::decode(&[2, 2, 1, 1]), None);
    assert_eq!(Ext::decode(&[200, 2, 1]), None);
}

#[test]
fn a_checkpoint_and_a_receipt_read_back_as_they_were_written() {
    let workspace = |n: u8| Workspace {
        id: [n; 8],
        last: 100 + n as u64,
        undoable: 90,
        index_at: 80,
        index_sum: [n; 20],
        step: 70,
    };
    let checkpoint = Checkpoint {
        boot: [5; 16],
        generation: 4,
        seq: 12,
        head: [6; 32],
        head_at: 500,
        head_end: 900,
        synced: 450,
        odb: OdbBounds {
            lens: [9, 8, 7, 6],
            generation: 2,
        },
        linked_at: 300,
        linked: 3,
        migrated: 64,
        required: 200,
        cut: 250,
        base: 0,
        workspaces: vec![workspace(1), workspace(2)],
    };
    for checkpoint in [checkpoint.clone(), Checkpoint::default()] {
        let body = checkpoint.encode();
        assert_eq!(Checkpoint::decode(&body), Some(checkpoint.clone()));
        assert_eq!(
            alt_oplog::v2::checkpoint::generation_of(&body),
            Some(checkpoint.generation)
        );
        for end in 0..body.len() {
            assert_eq!(Checkpoint::decode(&body[..end]), None, "cut at {end}");
        }
    }

    let mut receipt = Receipt {
        request: [1; 16],
        binding: [2; 32],
        outcome: Outcome::Refused,
        exit: 75,
        first_op: 640,
        ops: 3,
        finished_ms: 99,
        digest: [0; 32],
        len: 0,
        response: None,
    };
    for response in [vec![], b"staged 1 path(s)\n".to_vec(), vec![7; INLINE_MAX]] {
        receipt.answer(&response);
        let body = receipt.encode();
        assert_eq!(Receipt::decode(&body), Some(receipt.clone()));
        assert_eq!(receipt.response.as_deref(), Some(&response[..]));
        let mut other = body.clone();
        *other.last_mut().unwrap() ^= 1;
        assert_eq!(
            Receipt::decode(&other).is_none(),
            !response.is_empty(),
            "a response of another digest"
        );
    }
    // a long response is kept elsewhere: the receipt holds its digest and length
    receipt.answer(&vec![7; INLINE_MAX + 1]);
    assert_eq!(receipt.response, None);
    let body = receipt.encode();
    assert_eq!(body.len(), 107);
    assert_eq!(Receipt::decode(&body), Some(receipt));
}

#[test]
fn the_file_header_tells_what_a_build_may_do_with_the_log() {
    let plain = header(16);
    let bytes = plain.encode();
    assert_eq!(
        &bytes[..5],
        b"ALTL\x02",
        "an older alt refuses by the version"
    );
    assert_eq!(FileHeader::decode(&bytes).unwrap(), plain);
    assert!(plain.writable());
    assert_eq!(plain.layout().block, 65_536);

    let said = |header: FileHeader| match FileHeader::decode(&header.encode()) {
        Err(OpLogError::Format(said)) => said,
        other => panic!("{other:?}"),
    };
    let unknown = FileHeader {
        incompatible: 1 | 8,
        ..plain
    };
    assert_eq!(
        said(unknown),
        "the oplog uses a feature this alt does not know"
    );
    let read_only = FileHeader {
        read_compatible: 4,
        ..plain
    };
    assert!(!FileHeader::decode(&read_only.encode()).unwrap().writable());
    let ignored = FileHeader {
        compatible: 0x8000,
        ..plain
    };
    assert!(FileHeader::decode(&ignored.encode()).unwrap().writable());

    for at in 0..FILE_HEADER_LEN {
        let mut other = bytes;
        other[at] ^= 0x10;
        assert!(FileHeader::decode(&other).is_err(), "byte {at}");
    }
    assert!(FileHeader::decode(&bytes[..63]).is_err());
    let mut older = bytes;
    older[4] = 1;
    assert!(matches!(
        FileHeader::decode(&older),
        Err(OpLogError::Format("unsupported oplog version"))
    ));
}

#[test]
fn a_block_header_reads_only_as_the_header_of_its_own_block_and_log() {
    let layout = header(10).layout();
    let said = BlockHeader {
        index: 3,
        first: 32,
        checkpoint: 2080,
        generation: 1,
    };
    let bytes = layout.encode_header(&said);
    assert_eq!(layout.decode_header(&bytes, 3 << 10), Some(said));
    assert_eq!(layout.decode_header(&bytes, 2 << 10), None);
    let mut other = layout;
    other.id[0] ^= 1;
    assert_eq!(other.decode_header(&bytes, 3 << 10), None);
    for at in 0..32 {
        let mut damaged = bytes;
        damaged[at] ^= 1;
        assert_eq!(layout.decode_header(&damaged, 3 << 10), None, "byte {at}");
    }
}
