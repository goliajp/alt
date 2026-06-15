//! pkt-line: git's framing for every smart-protocol byte stream.
//!
//! ## Format
//!
//! Every line is `<4 hex bytes><payload>` where the hex is the total length
//! (header + payload) in ASCII. Special values:
//!
//! | hex   | meaning |
//! |-------|---------|
//! | `0000` | flush packet — ends a "request" or "command" group |
//! | `0001` | delimiter (protocol v2 only) — separates request sections |
//! | `0002` | response-end (protocol v2 only) — end of stateless response |
//! | `0004` | empty data packet (the four header bytes, no payload) |
//! | else  | a data packet, payload is `length - 4` bytes |
//!
//! Maximum length is `0xfff0` (65520). The pkt-line spec doesn't restrict
//! payload bytes — they're opaque to this layer; v2 commands give the
//! framed bytes their own line-based shape.
//!
//! ## API shape
//!
//! Readers receive [`Frame`]s and decide what to do; writers call one of
//! the four `write_…` helpers per frame. The reader doesn't allocate per
//! frame — the caller passes a reusable buffer — so a long advertisement
//! parses without churning the allocator.

use std::io::{Read, Write};

/// One pkt-line frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame<'a> {
    /// `<4 hex><payload>` with payload borrowed from the caller's buffer.
    Data(&'a [u8]),
    /// `0000` — section terminator.
    Flush,
    /// `0001` — section delimiter (v2 only).
    Delim,
    /// `0002` — stateless response end (v2 only).
    ResponseEnd,
}

/// Maximum pkt-line length (header + payload). Spec: `0xfff0` (65520).
pub const MAX_LINE_LEN: usize = 0xfff0;

/// Reasons a pkt-line stream fails to decode.
#[derive(Debug, thiserror::Error)]
pub enum PktError {
    #[error("io")]
    Io(#[from] std::io::Error),
    /// The 4-byte length prefix wasn't valid ASCII hex.
    #[error("length prefix is not valid hex: {0:?}")]
    BadLength([u8; 4]),
    /// The header claimed a length larger than `MAX_LINE_LEN` or smaller
    /// than the 4-byte header itself. A truthful claim from a peer's bug
    /// or an adversary trying to skew our reader.
    #[error("length prefix out of bounds: {0}")]
    LengthOutOfBounds(usize),
    /// The header claimed a length the stream cannot satisfy (e.g. a 1024-
    /// byte data line followed by EOF after 500 bytes).
    #[error("truncated pkt-line: header claimed {claimed} bytes, got {got}")]
    Truncated { claimed: usize, got: usize },
}

/// Read one pkt-line from `r` into `buf` (resized to fit). Returns the
/// frame, with [`Frame::Data`] borrowing from `buf`. EOF before the 4-byte
/// length header surfaces as `Err(PktError::Io)` with `kind = UnexpectedEof`
/// — peers terminate on a `Flush`, not mid-header.
pub fn read_frame<'b, R: Read>(r: &mut R, buf: &'b mut Vec<u8>) -> Result<Frame<'b>, PktError> {
    let mut hdr = [0u8; 4];
    r.read_exact(&mut hdr)?;
    let len = parse_hex_len(&hdr).ok_or(PktError::BadLength(hdr))?;
    match len {
        0 => Ok(Frame::Flush),
        1 => Ok(Frame::Delim),
        2 => Ok(Frame::ResponseEnd),
        3 => Err(PktError::LengthOutOfBounds(3)),
        l if l > MAX_LINE_LEN => Err(PktError::LengthOutOfBounds(l)),
        l => {
            // payload size = total - 4; len==4 is a legal empty data packet
            let payload = l - 4;
            buf.resize(payload, 0);
            // a truncated stream halfway through the payload should report
            // the partial read so a caller can diagnose, not crash
            match r.read_exact(buf) {
                Ok(()) => Ok(Frame::Data(buf)),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    Err(PktError::Truncated {
                        claimed: payload,
                        got: 0, // read_exact doesn't tell us how many it got; the diagnostic value is in `claimed`
                    })
                }
                Err(e) => Err(PktError::Io(e)),
            }
        }
    }
}

/// Write one data pkt-line: the 4-byte hex length header (`payload.len() + 4`)
/// followed by `payload`. Caller is responsible for `payload.len() <=
/// MAX_LINE_LEN - 4`.
pub fn write_data<W: Write>(w: &mut W, payload: &[u8]) -> std::io::Result<()> {
    assert!(
        payload.len() <= MAX_LINE_LEN - 4,
        "pkt-line payload exceeds MAX_LINE_LEN"
    );
    let total = (payload.len() + 4) as u16;
    w.write_all(&hex4(total as u32))?;
    w.write_all(payload)
}

/// Write a flush packet (`0000`).
pub fn write_flush<W: Write>(w: &mut W) -> std::io::Result<()> {
    w.write_all(b"0000")
}

/// Write a delimiter packet (`0001`, v2 only).
pub fn write_delim<W: Write>(w: &mut W) -> std::io::Result<()> {
    w.write_all(b"0001")
}

/// Write a response-end packet (`0002`, v2 only).
pub fn write_response_end<W: Write>(w: &mut W) -> std::io::Result<()> {
    w.write_all(b"0002")
}

fn parse_hex_len(b: &[u8; 4]) -> Option<usize> {
    let mut v = 0u32;
    for &c in b {
        let d = match c {
            b'0'..=b'9' => (c - b'0') as u32,
            b'a'..=b'f' => (c - b'a' + 10) as u32,
            b'A'..=b'F' => (c - b'A' + 10) as u32,
            _ => return None,
        };
        v = (v << 4) | d;
    }
    Some(v as usize)
}

fn hex4(v: u32) -> [u8; 4] {
    fn d(x: u32) -> u8 {
        match x {
            0..=9 => b'0' + x as u8,
            10..=15 => b'a' + (x - 10) as u8,
            _ => unreachable!(),
        }
    }
    [
        d((v >> 12) & 0xf),
        d((v >> 8) & 0xf),
        d((v >> 4) & 0xf),
        d(v & 0xf),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn read_one(bytes: &[u8]) -> Result<(Vec<u8>, Option<Vec<u8>>), PktError> {
        // returns (frame_kind_bytes, payload_if_data) so we can compare without
        // dealing with the Frame borrow lifetime in test data
        let mut r = Cursor::new(bytes);
        let mut buf = Vec::new();
        let f = read_frame(&mut r, &mut buf)?;
        Ok(match f {
            Frame::Flush => (b"flush".to_vec(), None),
            Frame::Delim => (b"delim".to_vec(), None),
            Frame::ResponseEnd => (b"end".to_vec(), None),
            Frame::Data(d) => (b"data".to_vec(), Some(d.to_vec())),
        })
    }

    #[test]
    fn special_packets_decode() {
        assert_eq!(read_one(b"0000").unwrap().0, b"flush");
        assert_eq!(read_one(b"0001").unwrap().0, b"delim");
        assert_eq!(read_one(b"0002").unwrap().0, b"end");
    }

    /// Length `0003` is reserved/invalid (too small to be a data packet, not
    /// special). A peer that sends it is buggy or hostile.
    #[test]
    fn length_three_is_rejected() {
        let err = read_one(b"0003").unwrap_err();
        assert!(matches!(err, PktError::LengthOutOfBounds(3)));
    }

    #[test]
    fn length_above_max_is_rejected() {
        // 0xfff1 > MAX_LINE_LEN (0xfff0)
        let err = read_one(b"fff1").unwrap_err();
        assert!(matches!(err, PktError::LengthOutOfBounds(0xfff1)));
    }

    /// Non-hex characters in the length header are a stream-format error,
    /// not silently truncated/zeroed.
    #[test]
    fn non_hex_length_is_rejected() {
        let err = read_one(b"00g0").unwrap_err();
        assert!(matches!(err, PktError::BadLength(_)));
    }

    /// An empty data packet (`0004`) is legal — it carries zero bytes of
    /// payload but is not a flush.
    #[test]
    fn empty_data_packet_decodes() {
        let (kind, payload) = read_one(b"0004").unwrap();
        assert_eq!(kind, b"data");
        assert_eq!(payload, Some(Vec::new()));
    }

    #[test]
    fn data_packet_round_trips() {
        let payload = b"hello\n";
        let mut buf = Vec::new();
        write_data(&mut buf, payload).unwrap();
        // 4 hex header bytes + 6 payload = 10 total. `0xa = 10`.
        assert_eq!(buf, b"000ahello\n");

        let (kind, decoded) = read_one(&buf).unwrap();
        assert_eq!(kind, b"data");
        assert_eq!(decoded, Some(payload.to_vec()));
    }

    #[test]
    fn round_trip_for_special_frames() {
        type Writer = fn(&mut Vec<u8>) -> std::io::Result<()>;
        let cases: [(Writer, &[u8]); 3] = [
            (write_flush, b"0000"),
            (write_delim, b"0001"),
            (write_response_end, b"0002"),
        ];
        for (write_fn, expect) in cases {
            let mut buf = Vec::new();
            write_fn(&mut buf).unwrap();
            assert_eq!(&buf[..], expect);
        }
    }

    /// A stream that promises 100 bytes of payload but EOFs at 50 surfaces
    /// as `Truncated` (or `Io::UnexpectedEof`) rather than panicking.
    #[test]
    fn truncated_payload_is_reported_not_panic() {
        // header claims 0x0008 = 8 bytes total → 4 bytes of payload,
        // but only 2 follow
        let bytes = b"0008XY";
        let r = read_one(bytes);
        assert!(r.is_err(), "expected error on truncated payload");
    }

    /// Sequential frames: write three, read back three.
    #[test]
    fn multiple_frames_stream() {
        let mut buf = Vec::new();
        write_data(&mut buf, b"first").unwrap();
        write_data(&mut buf, b"second").unwrap();
        write_flush(&mut buf).unwrap();

        let mut r = Cursor::new(&buf);
        let mut scratch = Vec::new();

        let f = read_frame(&mut r, &mut scratch).unwrap();
        assert_eq!(f, Frame::Data(b"first".as_ref()));
        let f = read_frame(&mut r, &mut scratch).unwrap();
        assert_eq!(f, Frame::Data(b"second".as_ref()));
        let f = read_frame(&mut r, &mut scratch).unwrap();
        assert_eq!(f, Frame::Flush);
    }

    /// Fuzz-level invariant: any byte string that pkt-line could legally
    /// emit (random-but-well-formed) round-trips losslessly. We don't fuzz
    /// arbitrary garbage here — that's the `is_err` boundary, and the
    /// boundary tests above cover the rejection axes.
    #[test]
    fn payloads_at_max_size_round_trip() {
        let big = vec![0x42u8; MAX_LINE_LEN - 4];
        let mut buf = Vec::new();
        write_data(&mut buf, &big).unwrap();
        let mut r = Cursor::new(&buf);
        let mut scratch = Vec::new();
        let f = read_frame(&mut r, &mut scratch).unwrap();
        match f {
            Frame::Data(d) => assert_eq!(d, &big[..]),
            other => panic!("expected Data, got {other:?}"),
        }
    }
}
