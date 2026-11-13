//! The gb18030 decoder (§10.2.1), which GBK decodes with too.
//!
//! Up to three bytes of a sequence wait in [`Gb18030Decoder`] for the
//! next chunk; the last chunk turns whatever is left into one error.

use alloc::vec::Vec;

use crate::cjk_index::{self, gb18030};
use crate::sink::{LONE, Sink};
use crate::{DecodeError, error, push_char};

/// The decoder's state between chunks: gb18030 first, second and
/// third, each 0x00 when unset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gb18030Decoder {
    first: u8,
    second: u8,
    third: u8,
}

impl Gb18030Decoder {
    /// The state as four bytes, for a caller that keeps it outside the
    /// decoder: first, second, third, then 0.
    pub fn to_bytes(self) -> [u8; 4] {
        [self.first, self.second, self.third, 0]
    }

    /// The state [`Self::to_bytes`] wrote. A byte the decoder could not
    /// have kept there (and every byte after it) reads as unset, and the
    /// fourth byte is ignored.
    pub fn from_bytes(b: [u8; 4]) -> Self {
        let first = if (0x81..=0xFE).contains(&b[0]) {
            b[0]
        } else {
            0
        };
        let second = if first != 0 && b[1].is_ascii_digit() {
            b[1]
        } else {
            0
        };
        let third = if second != 0 && (0x81..=0xFE).contains(&b[2]) {
            b[2]
        } else {
            0
        };
        Gb18030Decoder {
            first,
            second,
            third,
        }
    }

    /// Decode the chunk `input`, appending UTF-8 to `out`; `last` marks
    /// the end of the queue.
    pub fn decode(
        &mut self,
        input: &[u8],
        last: bool,
        fatal: bool,
        out: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        let mut s = Sink::new(out, input.len() + input.len() / 2);
        let n = input.len();
        let mut i = 0;
        loop {
            if *self == Gb18030Decoder::default() {
                while i < n {
                    let b = input[i];
                    if b.is_ascii() {
                        i = s.ascii_from::<LONE>(input, i, b);
                        continue;
                    }
                    // a two-byte sequence with an entry, in one step
                    if i + 1 < n
                        && (0x81..=0xFE).contains(&b)
                        && let cp @ 1.. = two_byte3(b, input[i + 1])
                    {
                        s.three(cp);
                        i += 2;
                        continue;
                    }
                    break;
                }
            }
            if i == n {
                break;
            }
            let b = input[i];
            i += 1;
            s.with_vec(|out| self.byte(b, fatal, out))?;
        }
        if last && *self != Gb18030Decoder::default() {
            *self = Gb18030Decoder::default();
            s.error(fatal)?;
        }
        Ok(())
    }

    /// The handler's steps for one byte. A byte the standard restores to
    /// the queue is handled again right after the error.
    fn byte(&mut self, b: u8, fatal: bool, out: &mut Vec<u8>) -> Result<(), DecodeError> {
        if self.third != 0 {
            let Gb18030Decoder {
                first,
                second,
                third,
            } = core::mem::take(self);
            if !b.is_ascii_digit() {
                error(out, fatal)?;
                for r in [second, third, b] {
                    self.byte(r, fatal, out)?;
                }
                return Ok(());
            }
            let pointer = u32::from(first - 0x81) * (10 * 126 * 10)
                + u32::from(second - 0x30) * (10 * 126)
                + u32::from(third - 0x81) * 10
                + u32::from(b - 0x30);
            return match cjk_index::gb18030_ranges(pointer) {
                Some(c) => {
                    push_char(out, c);
                    Ok(())
                }
                None => error(out, fatal),
            };
        }
        if self.second != 0 {
            if (0x81..=0xFE).contains(&b) {
                self.third = b;
                return Ok(());
            }
            let second = self.second;
            *self = Gb18030Decoder::default();
            error(out, fatal)?;
            self.byte(second, fatal, out)?;
            return self.byte(b, fatal, out);
        }
        if self.first != 0 {
            if b.is_ascii_digit() {
                self.second = b;
                return Ok(());
            }
            let leading = core::mem::take(&mut self.first);
            if let Some(c) = two_byte(leading, b) {
                push_char(out, c);
                return Ok(());
            }
            error(out, fatal)?;
            if b.is_ascii() {
                return self.byte(b, fatal, out);
            }
            return Ok(());
        }
        match b {
            0x00..=0x7F => out.push(b),
            0x80 => push_char(out, '\u{20AC}'),
            0x81..=0xFE => self.first = b,
            _ => error(out, fatal)?,
        }
        Ok(())
    }
}

/// The code point of the two-byte sequence `leading` (0x81 to 0xFE),
/// `b` when it is U+0800 or past, else 0.
#[inline(always)]
fn two_byte3(leading: u8, b: u8) -> u32 {
    let offset = if b < 0x7F { 0x40 } else { 0x41 };
    if !matches!(b, 0x40..=0x7E | 0x80..=0xFE) {
        return 0;
    }
    let pointer = usize::from(leading - 0x81) * 190 + usize::from(b - offset);
    // no entry is a surrogate, and 0 marks none
    const { assert!(gb18030::FIRST == 0) };
    match gb18030::CODE_POINTS.get(pointer) {
        Some(&unit @ 0x800..) => u32::from(unit),
        _ => 0,
    }
}

/// The code point of the two-byte sequence `leading` (0x81 to 0xFE), `b`.
fn two_byte(leading: u8, b: u8) -> Option<char> {
    let offset = if b < 0x7F { 0x40 } else { 0x41 };
    if !matches!(b, 0x40..=0x7E | 0x80..=0xFE) {
        return None;
    }
    let pointer = usize::from(leading - 0x81) * 190 + usize::from(b - offset);
    cjk_index::bmp(&gb18030::CODE_POINTS, gb18030::FIRST, pointer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    fn whole(input: &[u8], fatal: bool) -> Result<String, DecodeError> {
        let mut out = Vec::new();
        Gb18030Decoder::default().decode(input, true, fatal, &mut out)?;
        Ok(String::from_utf8(out).expect("utf-8 out"))
    }

    #[test]
    fn one_two_and_four_byte_sequences() {
        assert_eq!(
            whole(b"A\x80\xC4\xE3\xBA\xC3\x81\x30\x81\x30", true),
            Ok("A\u{20AC}\u{4F60}\u{597D}\u{80}".into())
        );
        // the last range, U+10000 and U+10FFFF
        assert_eq!(
            whole(b"\x90\x30\x81\x30\xE3\x32\x9A\x35", true),
            Ok("\u{10000}\u{10FFFF}".into())
        );
        // 0xA3 0xA0 is U+3000; pointer 7457 is U+E7C7
        assert_eq!(whole(b"\xA3\xA0", true), Ok("\u{3000}".into()));
        assert_eq!(whole(b"\x81\x35\xF4\x37", true), Ok("\u{E7C7}".into()));
    }

    #[test]
    fn errors_restore_the_bytes_the_standard_restores() {
        // a four-byte sequence broken at its last byte: second and third
        // go back, so the digit comes out and the third leads again
        assert_eq!(
            whole(b"\x81\x30\x81\x41", false),
            Ok("\u{FFFD}0\u{4E04}".into())
        );
        // broken at its third byte: second and the byte go back
        assert_eq!(whole(b"\x81\x30\x41", false), Ok("\u{FFFD}0A".into()));
        // a two-byte lead before an ASCII byte keeps the byte
        assert_eq!(whole(b"\x81\x7F", false), Ok("\u{FFFD}\u{7F}".into()));
        assert_eq!(whole(b"\x81\xFF", false), Ok("\u{FFFD}".into()));
        assert_eq!(whole(b"\xFF", false), Ok("\u{FFFD}".into()));
        // a pointer past the ranges
        assert_eq!(whole(b"\x84\x31\xA5\x30", false), Ok("\u{FFFD}".into()));
        assert_eq!(whole(b"\x84\x31\xA5\x30", true), Err(DecodeError));
        // an unfinished sequence at the end is one error
        assert_eq!(whole(b"A\x81\x30\x81", false), Ok("A\u{FFFD}".into()));
        assert_eq!(whole(b"\x81", true), Err(DecodeError));
    }

    #[test]
    fn a_chunk_boundary_splits_nothing() {
        let input = b"\x90\x30\x81\x30\xC4\xE3\x81\x30\x81\x41Z";
        let want = "\u{10000}\u{4F60}\u{FFFD}0\u{4E04}Z".as_bytes();
        for cut in 0..=input.len() {
            let mut d = Gb18030Decoder::default();
            let mut out = Vec::new();
            d.decode(&input[..cut], false, false, &mut out)
                .expect("decodes");
            let mut d = Gb18030Decoder::from_bytes(d.to_bytes());
            d.decode(&input[cut..], true, false, &mut out)
                .expect("decodes");
            assert_eq!(out, want, "cut at {cut}");
        }
    }

    #[test]
    fn from_bytes_keeps_only_a_state_the_decoder_can_be_in() {
        let d = Gb18030Decoder::from_bytes([0x81, 0x30, 0x81, 7]);
        assert_eq!(d.to_bytes(), [0x81, 0x30, 0x81, 0]);
        assert_eq!(
            Gb18030Decoder::from_bytes([0x81, 0x41, 0x81, 0]).to_bytes(),
            [0x81, 0, 0, 0]
        );
        assert_eq!(
            Gb18030Decoder::from_bytes([0x30, 0x30, 0x81, 0]),
            Gb18030Decoder::default()
        );
    }
}
