//! The single-byte decoder (§9.1) over the legacy single-byte indexes,
//! and the x-user-defined decoder (§14.5.1).
//!
//! Neither keeps state between chunks: every byte is a whole code
//! point.

use alloc::vec::Vec;

use crate::DecodeError;
use crate::sink::Sink;

/// ASCII bytes copied one at a time before a run reads words: in text
/// past ASCII written with Latin letters, ASCII runs between the other
/// letters are a few bytes long.
const SHORT_RUN: usize = 8;

/// A legacy single-byte index: the code points of bytes 0x80 to 0xFF.
#[derive(Debug)]
pub struct Index(pub(crate) [u16; 128]);

impl Index {
    /// The code point of `byte`, `None` where the index has no entry.
    pub fn code_point(&self, byte: u8) -> Option<char> {
        match byte {
            0x00..=0x7F => Some(char::from(byte)),
            // 0 marks no entry: no index maps a byte to U+0000
            _ => match self.0[usize::from(byte - 0x80)] {
                0 => None,
                cp => char::from_u32(u32::from(cp)),
            },
        }
    }
}

/// Decode `input` with `index`, appending UTF-8 to `out`. A byte the
/// index has no entry for is an error.
pub fn decode(
    index: &Index,
    input: &[u8],
    fatal: bool,
    out: &mut Vec<u8>,
) -> Result<(), DecodeError> {
    let mut s = Sink::new(out, input.len() + input.len() / 2);
    let mut i = 0;
    while let Some(&b) = input.get(i) {
        if b.is_ascii() {
            i = s.ascii_from::<SHORT_RUN>(input, i, b);
            continue;
        }
        i += 1;
        // 0 marks no entry, and every entry is past ASCII and in the
        // BMP but not a surrogate
        match index.0[usize::from(b & 0x7F)] {
            0 => s.error(fatal)?,
            cp => s.past_ascii(u32::from(cp)),
        }
    }
    Ok(())
}

/// Decode `input` as x-user-defined: ASCII as itself, each byte 0x80 to
/// 0xFF as U+F780 to U+F7FF. It never errs.
pub fn decode_x_user_defined(input: &[u8], out: &mut Vec<u8>) {
    let mut s = Sink::new(out, input.len());
    let mut i = 0;
    while let Some(&b) = input.get(i) {
        if b.is_ascii() {
            i = s.ascii_from::<SHORT_RUN>(input, i, b);
            continue;
        }
        i += 1;
        s.past_ascii(0xF780 + u32::from(b - 0x80));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Encoding;
    use alloc::string::String;

    fn decoded(e: Encoding, input: &[u8], fatal: bool) -> Result<String, DecodeError> {
        let mut out = Vec::new();
        let index = e.single_byte_index().expect("a single-byte encoding");
        decode(index, input, fatal, &mut out)?;
        Ok(String::from_utf8(out).expect("utf-8 out"))
    }

    #[test]
    fn every_single_byte_encoding_has_an_index() {
        let single = (Encoding::ALL.iter()).filter(|e| e.single_byte_index().is_some());
        assert_eq!(single.count(), 28);
        assert!(Encoding::Utf8.single_byte_index().is_none());
        assert!(Encoding::XUserDefined.single_byte_index().is_none());
        // ISO-8859-8-I decodes as ISO-8859-8
        let (a, b) = (Encoding::Iso8859_8, Encoding::Iso8859_8I);
        assert!(core::ptr::eq(
            a.single_byte_index().unwrap(),
            b.single_byte_index().unwrap()
        ));
    }

    #[test]
    fn every_entry_is_past_ascii_and_no_surrogate() {
        for e in Encoding::ALL {
            if let Some(index) = e.single_byte_index() {
                let ok = |&cp: &u16| cp == 0 || (cp >= 0x80 && !(0xD800..=0xDFFF).contains(&cp));
                assert!(index.0.iter().all(ok), "{e:?}");
            }
        }
    }

    #[test]
    fn indexes_hold_the_standards_code_points() {
        let e = Encoding::Windows1252;
        assert_eq!(
            decoded(e, b"A\x80\x81\x9F\xA0\xFF", false),
            Ok("A\u{20AC}\u{81}\u{178}\u{A0}\u{FF}".into())
        );
        assert_eq!(
            decoded(Encoding::Koi8R, b"\xC1\xFF", false),
            Ok("\u{430}\u{42A}".into())
        );
        assert_eq!(
            decoded(Encoding::Ibm866, b"\x80\xFF", false),
            Ok("\u{410}\u{A0}".into())
        );
        assert_eq!(
            decoded(Encoding::Macintosh, b"\x80\xF0", false),
            Ok("\u{C4}\u{F8FF}".into())
        );
    }

    #[test]
    fn a_byte_without_an_entry_is_an_error() {
        // windows-874 has no entry for 0xDB
        let e = Encoding::Windows874;
        assert_eq!(decoded(e, b"a\xDBb", false), Ok("a\u{FFFD}b".into()));
        assert_eq!(decoded(e, b"a\xDBb", true), Err(DecodeError));
        assert_eq!(decoded(e, b"a\xA1b", true), Ok("a\u{E01}b".into()));
    }

    #[test]
    fn x_user_defined_maps_high_bytes_to_private_use() {
        let mut out = Vec::new();
        decode_x_user_defined(b"A\x80\xFF", &mut out);
        assert_eq!(out, "A\u{F780}\u{F7FF}".as_bytes());
    }
}
