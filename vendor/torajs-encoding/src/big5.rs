//! The Big5 decoder (§11.1.1).
//!
//! A leading byte waits in [`Big5Decoder`] for the next chunk; the
//! last chunk turns one left over into an error.

use alloc::vec::Vec;

use crate::DecodeError;
use crate::cjk_index;
use crate::lead::{NO_COLUMN, Pair, Single, columns, lead_decoder};

lead_decoder! {
    /// The decoder's state between chunks: Big5 leading.
    Big5Decoder, is_lead, single, pair3, pair
}

#[inline(always)]
fn is_lead(b: u8) -> bool {
    (0x81..=0xFE).contains(&b)
}

#[inline(always)]
fn single(b: u8) -> Single {
    match b {
        0x00..=0x7F => Single::Char(char::from(b)),
        _ => Single::Error,
    }
}

/// The trailing bytes' columns, 157 to a row.
static COLUMNS: [u8; 256] = columns(&[(0x40, 0x7E), (0xA1, 0xFE)]);

/// `COLUMNS` as offsets that, added to a leading byte times 157, give
/// the index into the stored entries: wrapping below the first pointer,
/// and past every entry for a byte that cannot trail.
static OFFSETS: [u16; 256] = {
    let mut table = [u16::MAX; 256];
    let mut b = 0;
    while b < 256 {
        if COLUMNS[b] != NO_COLUMN {
            table[b] = (COLUMNS[b] as u16).wrapping_sub(0x81 * 157 + cjk_index::big5::FIRST as u16);
        }
        b += 1;
    }
    table
};

#[inline(always)]
fn pair3(leading: u8, b: u8) -> u32 {
    let at = (u32::from(leading) * 157 + u32::from(OFFSETS[usize::from(b)])) & 0xFFFF;
    match cjk_index::big5::CODE_POINTS.get(at as usize) {
        // a surrogate stands for a plane 2 entry
        Some(&unit @ 0x800..) if unit & 0xF800 != 0xD800 => u32::from(unit),
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
    let pointer = usize::from(leading - 0x81) * 157 + usize::from(column);
    if let Some(c) = cjk_index::big5(pointer) {
        return Some((c, None));
    }
    // the pointers no single code point stands for, which the index
    // leaves empty
    let (c, combining) = match pointer {
        1133 => ('\u{CA}', '\u{304}'),
        1135 => ('\u{CA}', '\u{30C}'),
        1164 => ('\u{EA}', '\u{304}'),
        1166 => ('\u{EA}', '\u{30C}'),
        _ => return None,
    };
    Some((c, Some(combining)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    fn whole(input: &[u8], fatal: bool) -> Result<String, DecodeError> {
        let mut out = Vec::new();
        Big5Decoder::default().decode(input, true, fatal, &mut out)?;
        Ok(String::from_utf8(out).expect("utf-8 out"))
    }

    #[test]
    fn two_byte_sequences() {
        // 中文, the first pointer (942), the four pointers of two code
        // points, a plane 2 entry (pointer 975)
        assert_eq!(
            whole(b"A\xA4\xA4\xA4\xE5\x87\x40", true),
            Ok("A\u{4E2D}\u{6587}\u{43F0}".into())
        );
        assert_eq!(
            whole(b"\x88\x62\x88\x64", true),
            Ok("\u{CA}\u{304}\u{CA}\u{30C}".into())
        );
        assert_eq!(
            whole(b"\x88\xA3\x88\xA5", true),
            Ok("\u{EA}\u{304}\u{EA}\u{30C}".into())
        );
        assert_eq!(whole(b"\x87\x61", true), Ok("\u{21A63}".into()));
    }

    #[test]
    fn the_index_leaves_the_pointers_of_two_code_points_empty() {
        assert!(
            [1133, 1135, 1164, 1166]
                .iter()
                .all(|&p| cjk_index::big5(p).is_none())
        );
    }

    #[test]
    fn errors_keep_an_ascii_trail() {
        assert_eq!(whole(b"\xA4\x7F", false), Ok("\u{FFFD}\u{7F}".into()));
        // a pointer without an entry, its ASCII trail restored
        assert_eq!(
            whole(b"\x80\x81\x40", false),
            Ok("\u{FFFD}\u{FFFD}@".into())
        );
        assert_eq!(whole(b"\xA4\xFF", false), Ok("\u{FFFD}".into()));
        assert_eq!(whole(b"\xA4", false), Ok("\u{FFFD}".into()));
        assert_eq!(whole(b"\xA4", true), Err(DecodeError));
    }

    #[test]
    fn the_longest_outputs_fit_the_room_reserved() {
        // three bytes per byte and four per pair; the sink asserts every
        // store fits in a debug build
        let len = |input: &[u8], fatal| whole(input, fatal).map(|s| s.len());
        assert_eq!(len(&[0xFF; 64], false), Ok(192));
        assert_eq!(len(&b"\x88\x62".repeat(32), true), Ok(128));
        assert_eq!(len(&b"\xA4\x7F".repeat(32), false), Ok(128));
        assert_eq!(len(&b"\x87\x61".repeat(32), true), Ok(128));
        // a carried leading byte's trail writes four, then three per byte
        let mut chunk = Vec::from([0x62]);
        chunk.extend([0xFF; 15]);
        let mut d = Big5Decoder::from_bytes([0x88, 0, 0, 0]);
        let mut out = Vec::new();
        d.decode(&chunk, true, false, &mut out).expect("not fatal");
        assert_eq!(out.len(), 4 + 3 * 15);
    }

    #[test]
    fn a_chunk_boundary_splits_nothing() {
        let input = b"\xA4\xA4\x88\x62\x87\x61";
        for cut in 0..=input.len() {
            let mut d = Big5Decoder::default();
            let mut out = Vec::new();
            d.decode(&input[..cut], false, true, &mut out)
                .expect("decodes");
            let mut d = Big5Decoder::from_bytes(d.to_bytes());
            d.decode(&input[cut..], true, true, &mut out)
                .expect("decodes");
            assert_eq!(
                out,
                "\u{4E2D}\u{CA}\u{304}\u{21A63}".as_bytes(),
                "cut at {cut}"
            );
        }
        assert_eq!(
            Big5Decoder::from_bytes([0x41, 1, 2, 3]),
            Big5Decoder::default()
        );
    }
}
