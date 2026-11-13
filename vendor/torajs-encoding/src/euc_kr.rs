//! The EUC-KR decoder (§13.1.1).
//!
//! A leading byte waits in [`EucKrDecoder`] for the next chunk; the
//! last chunk turns one left over into an error.

use alloc::vec::Vec;

use crate::DecodeError;
use crate::cjk_index::{self, euc_kr};
use crate::lead::{Pair, Single, lead_decoder};

lead_decoder! {
    /// The decoder's state between chunks: EUC-KR leading.
    EucKrDecoder, is_lead, single, pair3, pair
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

#[inline(always)]
fn pair3(leading: u8, b: u8) -> u32 {
    if !(0x41..=0xFE).contains(&b) {
        return 0;
    }
    let pointer = usize::from(leading - 0x81) * 190 + usize::from(b - 0x41);
    // no entry is a surrogate, and 0 marks none
    const { assert!(euc_kr::FIRST == 0) };
    match euc_kr::CODE_POINTS.get(pointer) {
        Some(&unit @ 0x800..) => u32::from(unit),
        _ => 0,
    }
}

#[cold]
#[inline(never)]
fn pair(leading: u8, b: u8) -> Pair {
    if !(0x41..=0xFE).contains(&b) {
        return None;
    }
    let pointer = usize::from(leading - 0x81) * 190 + usize::from(b - 0x41);
    cjk_index::bmp(&euc_kr::CODE_POINTS, euc_kr::FIRST, pointer).map(|c| (c, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    fn whole(input: &[u8], fatal: bool) -> Result<String, DecodeError> {
        let mut out = Vec::new();
        EucKrDecoder::default().decode(input, true, fatal, &mut out)?;
        Ok(String::from_utf8(out).expect("utf-8 out"))
    }

    #[test]
    fn two_byte_sequences() {
        // 한국어, the first pointer (0x81 0x41), and a UHC extension
        assert_eq!(
            whole(b"A\xC7\xD1\xB1\xB9\xBE\xEE\x81\x41", true),
            Ok("A\u{D55C}\u{AD6D}\u{C5B4}\u{AC02}".into())
        );
    }

    #[test]
    fn errors_keep_an_ascii_trail() {
        assert_eq!(whole(b"\x81\x40", false), Ok("\u{FFFD}@".into()));
        assert_eq!(whole(b"\x80\xFF", false), Ok("\u{FFFD}\u{FFFD}".into()));
        assert_eq!(whole(b"\xC7\xFF", false), Ok("\u{FFFD}".into()));
        assert_eq!(whole(b"\xC7", false), Ok("\u{FFFD}".into()));
        assert_eq!(whole(b"\xC7", true), Err(DecodeError));
    }

    #[test]
    fn a_chunk_boundary_splits_nothing() {
        let input = b"\xC7\xD1Z\xB1\xB9";
        for cut in 0..=input.len() {
            let mut d = EucKrDecoder::default();
            let mut out = Vec::new();
            d.decode(&input[..cut], false, true, &mut out)
                .expect("decodes");
            let mut d = EucKrDecoder::from_bytes(d.to_bytes());
            d.decode(&input[cut..], true, true, &mut out)
                .expect("decodes");
            assert_eq!(out, "\u{D55C}Z\u{AD6D}".as_bytes(), "cut at {cut}");
        }
    }
}
