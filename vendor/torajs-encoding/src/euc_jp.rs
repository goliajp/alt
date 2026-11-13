//! The EUC-JP decoder (§12.1.1).
//!
//! A leading byte (and whether it follows 0x8F, which reads the next
//! byte pair in index jis0212) waits in [`EucJpDecoder`] for the next
//! chunk; the last chunk turns one left over into an error.

use alloc::vec::Vec;

use crate::cjk_index::{self, jis0212};
use crate::sink::{LONE, Sink};
use crate::{DecodeError, error, push_char};

/// The decoder's state between chunks: EUC-JP jis0212 and EUC-JP
/// leading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EucJpDecoder {
    /// The leading byte, 0x00 when unset.
    lead: u8,
    /// The leading byte followed 0x8F.
    jis0212: bool,
}

impl EucJpDecoder {
    /// The state as four bytes, for a caller that keeps it outside the
    /// decoder: the leading byte (0 for none), 1 if it followed 0x8F
    /// else 0, then two zeros.
    pub fn to_bytes(self) -> [u8; 4] {
        [self.lead, u8::from(self.jis0212), 0, 0]
    }

    /// The state [`Self::to_bytes`] wrote. A first byte that cannot lead
    /// reads as none, the second byte counts only after a leading byte
    /// in 0xA1 to 0xFE, and the other two are ignored.
    pub fn from_bytes(b: [u8; 4]) -> Self {
        let lead = if matches!(b[0], 0x8E | 0x8F | 0xA1..=0xFE) {
            b[0]
        } else {
            0
        };
        EucJpDecoder {
            lead,
            jis0212: b[1] != 0 && lead >= 0xA1,
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
            if self.lead == 0 {
                while i < n {
                    let b = input[i];
                    if b.is_ascii() {
                        i = s.ascii_from::<LONE>(input, i, b);
                        continue;
                    }
                    // a jis0208 pair with an entry, in one step
                    if i + 1 < n
                        && let (leading @ 0xA1..=0xFE, b @ 0xA1..=0xFE) = (b, input[i + 1])
                        && let cp @ 1.. = jis0208_pair3(leading, b)
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
        if last && self.lead != 0 {
            // the standard leaves jis0212 as it was; no byte reads it
            // before a leading byte that follows 0x8F sets it again
            *self = EucJpDecoder::default();
            s.error(fatal)?;
        }
        Ok(())
    }

    /// The handler's steps for one byte.
    fn byte(&mut self, b: u8, fatal: bool, out: &mut Vec<u8>) -> Result<(), DecodeError> {
        if self.lead == 0x8E && (0xA1..=0xDF).contains(&b) {
            self.lead = 0;
            // SAFETY: U+FF61 to U+FF9F are scalar values
            push_char(out, unsafe {
                char::from_u32_unchecked(0xFF61 - 0xA1 + u32::from(b))
            });
            return Ok(());
        }
        if self.lead == 0x8F && (0xA1..=0xFE).contains(&b) {
            self.jis0212 = true;
            self.lead = b;
            return Ok(());
        }
        if self.lead != 0 {
            let leading = core::mem::take(&mut self.lead);
            let jis0212 = core::mem::take(&mut self.jis0212);
            let mut code_point = None;
            if (0xA1..=0xFE).contains(&leading) && (0xA1..=0xFE).contains(&b) {
                let pointer = usize::from(leading - 0xA1) * 94 + usize::from(b - 0xA1);
                code_point = if jis0212 {
                    cjk_index::bmp(&jis0212::CODE_POINTS, jis0212::FIRST, pointer)
                } else {
                    cjk_index::jis0208(pointer)
                };
            }
            if let Some(c) = code_point {
                push_char(out, c);
                return Ok(());
            }
            error(out, fatal)?;
            // restored, an ASCII byte decodes as itself
            if b.is_ascii() {
                out.push(b);
            }
            return Ok(());
        }
        match b {
            0x00..=0x7F => out.push(b),
            0x8E | 0x8F | 0xA1..=0xFE => self.lead = b,
            _ => error(out, fatal)?,
        }
        Ok(())
    }
}

/// The jis0208 code point of the pair `leading`, `b`, both 0xA1 to
/// 0xFE, when it is U+0800 or past, else 0.
#[inline(always)]
fn jis0208_pair3(leading: u8, b: u8) -> u32 {
    let pointer = usize::from(leading - 0xA1) * 94 + usize::from(b - 0xA1);
    // no entry is a surrogate, and 0 marks none
    const { assert!(cjk_index::jis0208::FIRST == 0) };
    match cjk_index::jis0208::CODE_POINTS.get(pointer) {
        Some(&unit @ 0x800..) => u32::from(unit),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    fn whole(input: &[u8], fatal: bool) -> Result<String, DecodeError> {
        let mut out = Vec::new();
        EucJpDecoder::default().decode(input, true, fatal, &mut out)?;
        Ok(String::from_utf8(out).expect("utf-8 out"))
    }

    #[test]
    fn jis0208_katakana_and_jis0212() {
        // 日本語, halfwidth katakana, then jis0212's first entry (˘)
        assert_eq!(
            whole(
                b"A\xC6\xFC\xCB\xDC\xB8\xEC\x8E\xA1\x8E\xDF\x8F\xA2\xAF",
                true
            ),
            Ok("A\u{65E5}\u{672C}\u{8A9E}\u{FF61}\u{FF9F}\u{2D8}".into())
        );
    }

    #[test]
    fn errors_keep_an_ascii_trail() {
        assert_eq!(whole(b"\xC6\x41", false), Ok("\u{FFFD}A".into()));
        // 0x8E before a byte past the katakana, 0x8F before a byte that
        // cannot lead
        assert_eq!(whole(b"\x8E\xE0", false), Ok("\u{FFFD}".into()));
        assert_eq!(whole(b"\x8F\x41", false), Ok("\u{FFFD}A".into()));
        // jis0212 without an entry
        assert_eq!(whole(b"\x8F\xA1\xA1", false), Ok("\u{FFFD}".into()));
        assert_eq!(whole(b"\x80\xFF", false), Ok("\u{FFFD}\u{FFFD}".into()));
        assert_eq!(whole(b"\x8F\xA2", false), Ok("\u{FFFD}".into()));
        assert_eq!(whole(b"\xC6", true), Err(DecodeError));
    }

    #[test]
    fn a_chunk_boundary_splits_nothing() {
        let input = b"\x8F\xA2\xAF\xC6\xFC\x8E\xB1";
        for cut in 0..=input.len() {
            let mut d = EucJpDecoder::default();
            let mut out = Vec::new();
            d.decode(&input[..cut], false, true, &mut out)
                .expect("decodes");
            let mut d = EucJpDecoder::from_bytes(d.to_bytes());
            d.decode(&input[cut..], true, true, &mut out)
                .expect("decodes");
            assert_eq!(out, "\u{2D8}\u{65E5}\u{FF71}".as_bytes(), "cut at {cut}");
        }
        assert_eq!(
            EucJpDecoder::from_bytes([0x8F, 1, 0, 0]).to_bytes(),
            [0x8F, 0, 0, 0]
        );
    }
}
