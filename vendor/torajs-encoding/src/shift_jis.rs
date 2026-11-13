//! The Shift_JIS decoder (§12.3.1).
//!
//! A leading byte waits in [`ShiftJisDecoder`] for the next chunk; the
//! last chunk turns one left over into an error.

use alloc::vec::Vec;

use crate::DecodeError;
use crate::cjk_index;
use crate::lead::{NO_COLUMN, Pair, Single, columns, lead_decoder};

lead_decoder! {
    /// The decoder's state between chunks: Shift_JIS leading.
    ShiftJisDecoder, is_lead, single, pair3, pair
}

/// The leading bytes' rows, then the trailing bytes' columns, 188 to a
/// row.
static ROWS: [u8; 256] = columns(&[(0x81, 0x9F), (0xE0, 0xFC)]);
static COLUMNS: [u8; 256] = columns(&[(0x40, 0x7E), (0x80, 0xFC)]);

#[inline(always)]
fn is_lead(b: u8) -> bool {
    ROWS[usize::from(b)] != NO_COLUMN
}

#[inline(always)]
fn single(b: u8) -> Single {
    match b {
        0x00..=0x80 => Single::Char(char::from(b)),
        // SAFETY: U+FF61 to U+FF9F are scalar values
        0xA1..=0xDF => {
            Single::Char(unsafe { char::from_u32_unchecked(0xFF61 - 0xA1 + u32::from(b)) })
        }
        _ => Single::Error,
    }
}

#[inline(always)]
fn pair3(leading: u8, b: u8) -> u32 {
    let column = COLUMNS[usize::from(b)];
    if column == NO_COLUMN {
        return 0;
    }
    let pointer = usize::from(ROWS[usize::from(leading)]) * 188 + usize::from(column);
    // no entry is a surrogate, and 0 marks none
    const { assert!(cjk_index::jis0208::FIRST == 0) };
    match cjk_index::jis0208::CODE_POINTS.get(pointer) {
        Some(&unit @ 0x800..) => u32::from(unit),
        _ => 0,
    }
}

#[cold]
#[inline(never)]
fn pair(leading: u8, b: u8) -> Pair {
    let column = COLUMNS[usize::from(b)];
    if column == NO_COLUMN {
        return None;
    }
    let pointer = usize::from(ROWS[usize::from(leading)]) * 188 + usize::from(column);
    match cjk_index::jis0208(pointer) {
        Some(c) => Some((c, None)),
        // the EUDC rows, which the index leaves empty, as Windows
        // decodes them: to private use
        None if (8836..=10715).contains(&pointer) => {
            // SAFETY: U+E000 to U+E757 are scalar values
            let c = unsafe { char::from_u32_unchecked(0xE000 - 8836 + pointer as u32) };
            Some((c, None))
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    fn whole(input: &[u8], fatal: bool) -> Result<String, DecodeError> {
        let mut out = Vec::new();
        ShiftJisDecoder::default().decode(input, true, fatal, &mut out)?;
        Ok(String::from_utf8(out).expect("utf-8 out"))
    }

    #[test]
    fn single_and_double_bytes() {
        // ASCII and 0x80 as themselves, halfwidth katakana, 日本語
        assert_eq!(
            whole(b"\\~\x80\xA1\xDF\x93\xFA\x96\x7B\x8C\xEA", true),
            Ok("\\~\u{80}\u{FF61}\u{FF9F}\u{65E5}\u{672C}\u{8A9E}".into())
        );
        // the first and last EUDC pointers
        assert_eq!(
            whole(b"\xF0\x40\xF9\xFC", true),
            Ok("\u{E000}\u{E757}".into())
        );
    }

    #[test]
    fn the_index_leaves_the_eudc_rows_empty() {
        assert!((8836..=10715).all(|p| cjk_index::jis0208(p).is_none()));
    }

    #[test]
    fn errors_keep_an_ascii_trail() {
        assert_eq!(whole(b"\x81\x7F", false), Ok("\u{FFFD}\u{7F}".into()));
        assert_eq!(whole(b"\x81\xFD", false), Ok("\u{FFFD}".into()));
        assert_eq!(
            whole(b"\xA0\xFD\xFE\xFF", false),
            Ok("\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}".into())
        );
        assert_eq!(whole(b"\x93", false), Ok("\u{FFFD}".into()));
        assert_eq!(whole(b"\x93", true), Err(DecodeError));
    }

    #[test]
    fn a_chunk_boundary_splits_nothing() {
        let input = b"\x93\xFA\xB1\x96\x7B";
        for cut in 0..=input.len() {
            let mut d = ShiftJisDecoder::default();
            let mut out = Vec::new();
            d.decode(&input[..cut], false, true, &mut out)
                .expect("decodes");
            let mut d = ShiftJisDecoder::from_bytes(d.to_bytes());
            d.decode(&input[cut..], true, true, &mut out)
                .expect("decodes");
            assert_eq!(out, "\u{65E5}\u{FF71}\u{672C}".as_bytes(), "cut at {cut}");
        }
        assert_eq!(
            ShiftJisDecoder::from_bytes([0xA0, 0, 0, 0]),
            ShiftJisDecoder::default()
        );
    }
}
