use alt_zstd::{Error, decode_all, decode_with_prefix, encode_all};

use crate::inputs::{random, text};
use crate::reference;

#[test]
fn empty_input_is_not_a_frame() {
    assert_eq!(decode_all(&[]), Err(Error::Truncated));
    assert!(reference::decode(&[]).is_err());
    assert_eq!(decode_with_prefix(&[], b"prefix", 0), Err(Error::Truncated));
}

#[test]
fn concatenated_and_skippable_frames() {
    let (a, b, c) = (text(70_000, 31), random(900, 32), text(200_000, 33));
    let mut stream = Vec::new();
    stream.extend_from_slice(&reference::encode_checked(&a, 3));
    // a skippable frame: magic, length, payload
    stream.extend_from_slice(&[0x53, 0x2A, 0x4D, 0x18, 5, 0, 0, 0, 1, 2, 3, 4, 5]);
    stream.extend_from_slice(&encode_all(&b, 1));
    stream.extend_from_slice(&[0x50, 0x2A, 0x4D, 0x18, 0, 0, 0, 0]);
    stream.extend_from_slice(&reference::encode(&c, 19));
    stream.extend_from_slice(&encode_all(&[], 3));
    stream.extend_from_slice(&reference::encode(&[], 3));
    let expected = [a, b, c].concat();
    assert!(decode_all(&stream).as_ref() == Ok(&expected));
    assert!(reference::decode(&stream).unwrap() == expected);

    // a later frame may not reach back into an earlier one: decoding the
    // tail alone must give the same bytes as decoding it in the stream
    let tail = reference::encode(&expected[70_900..], 19);
    assert!(decode_all(&tail).unwrap() == expected[70_900..]);
}

#[test]
fn trailing_bytes_are_rejected() {
    let mut frame = encode_all(b"some bytes some bytes some bytes", 3);
    frame.push(0);
    assert_eq!(decode_all(&frame), Err(Error::Truncated));
    frame.extend_from_slice(&[1, 2, 3]);
    assert_eq!(decode_all(&frame), Err(Error::BadMagic));
    assert!(reference::decode(&frame).is_err());
}

#[test]
fn checksums_are_verified() {
    let data = text(50_000, 34);
    let mut frame = reference::encode_checked(&data, 3);
    assert!(decode_all(&frame).as_ref() == Ok(&data));
    let last = frame.len() - 1;
    frame[last] ^= 0x10;
    assert_eq!(decode_all(&frame), Err(Error::Checksum));
    frame.truncate(last);
    assert_eq!(decode_all(&frame), Err(Error::Truncated));
}

#[test]
fn checksums_of_all_tail_lengths() {
    // every remainder of the 32-byte stripe and of the 8- and 4-byte steps
    let data = random(200, 35);
    for len in 0..data.len() {
        let frame = reference::encode_checked(&data[..len], 1);
        assert_eq!(decode_all(&frame).as_deref(), Ok(&data[..len]), "{len}");
    }
}

#[test]
fn dictionary_and_reserved_bits_are_unsupported() {
    // descriptor: single segment, 1-byte dictionary id; id 7; size 1; raw block
    let frame = [0x28, 0xB5, 0x2F, 0xFD, 0x21, 7, 1, 0x09, 0, 0, b'x'];
    assert_eq!(decode_all(&frame), Err(Error::Unsupported));
    // the same with id 0, which means no dictionary
    let frame = [0x28, 0xB5, 0x2F, 0xFD, 0x21, 0, 1, 0x09, 0, 0, b'x'];
    assert_eq!(decode_all(&frame), Ok(vec![b'x']));
    // reserved descriptor bit
    let frame = [0x28, 0xB5, 0x2F, 0xFD, 0x28, 1, 0x09, 0, 0, b'x'];
    assert_eq!(decode_all(&frame), Err(Error::Unsupported));
}

#[test]
fn declared_sizes_must_hold() {
    let data = text(300, 36);
    let frame = encode_all(&data, 3);
    // single segment with a 2-byte content size right after the descriptor
    assert_eq!(frame[4] >> 5, 0b011);
    for delta in [1u8, 0xFF] {
        let mut wrong = frame.clone();
        wrong[5] = wrong[5].wrapping_add(delta);
        assert!(decode_all(&wrong).is_err(), "{delta}");
        assert!(reference::decode(&wrong).is_err());
    }
    let mut longer = frame.clone();
    longer[5] += 1;
    assert_eq!(decode_all(&longer), Err(Error::SizeMismatch));
    assert_eq!(decode_with_prefix(&frame, &[], 300).as_ref(), Ok(&data));
    assert_eq!(
        decode_with_prefix(&frame, &[], 299),
        Err(Error::SizeMismatch)
    );
    assert_eq!(
        decode_with_prefix(&frame, &[], 301),
        Err(Error::SizeMismatch)
    );
}

#[test]
fn huge_declared_size_does_not_allocate() {
    // 8-byte content size of 2^60 in front of one small raw block
    let mut frame = vec![0x28, 0xB5, 0x2F, 0xFD, 0xE0];
    frame.extend_from_slice(&(1u64 << 60).to_le_bytes());
    frame.extend_from_slice(&[0x09, 0, 0, b'x']);
    assert_eq!(decode_all(&frame), Err(Error::SizeMismatch));

    // a window descriptor of the largest size with no content size: blocks
    // are still limited to 128 KiB each and nothing is reserved up front
    let frame = [0x28, 0xB5, 0x2F, 0xFD, 0x00, 0xFF, 0x09, 0, 0, b'x'];
    assert_eq!(decode_all(&frame), Ok(vec![b'x']));

    // single-byte blocks expand 4 bytes into 128 KiB each; the caller's
    // expected length stops that early
    let mut frame = vec![0x28, 0xB5, 0x2F, 0xFD, 0x00, 0x58];
    for _ in 0..1000 {
        frame.extend_from_slice(&[0xFA, 0xFF, 0x0F, 0]);
    }
    frame.extend_from_slice(&[0x01, 0, 0]);
    assert_eq!(
        decode_with_prefix(&frame, &[], 4096),
        Err(Error::SizeMismatch)
    );
    assert_eq!(decode_all(&frame).map(|out| out.len()), Ok(1000 * 0x1FFFF));
}
