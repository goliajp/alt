//! The WHATWG Encoding Standard's encodings and labels, with decoders
//! to UTF-8 for every encoding of the standard but UTF-8 itself and the
//! replacement encoding: the legacy single-byte encodings, the legacy
//! multi-byte Chinese (GBK, gb18030, Big5), Japanese (EUC-JP,
//! ISO-2022-JP, Shift_JIS) and Korean (EUC-KR) encodings,
//! x-user-defined and UTF-16LE / UTF-16BE.
//!
//! A decoder appends UTF-8 to a `Vec<u8>` and answers [`DecodeError`]
//! where the standard's decoder returns error and the caller asked to
//! be fatal; otherwise it writes U+FFFD there and goes on (the
//! standard's error modes, §4.1). What `TextDecoder` layers on top
//! (BOM stripping, the do-not-flush reset) is the caller's.
//!
//! Every index is its own static and every decoder its own function,
//! so a program links only the ones it names. A decoder that keeps
//! state between chunks is a small `Copy` type with `decode`, and
//! `to_bytes` / `from_bytes` for a caller that keeps the state in four
//! bytes of its own.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub mod big5;
mod cjk_index;
mod encoding;
pub mod euc_jp;
pub mod euc_kr;
pub mod gb18030;
pub mod index;
pub mod iso_2022_jp;
mod label;
mod lead;
pub mod shift_jis;
pub mod single_byte;
mod sink;
pub mod utf16;

pub use encoding::Encoding;

/// Which decoder an encoding decodes with.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum Family {
    /// UTF-8, whose decoder `core::str::from_utf8` already is.
    Utf8,
    /// [`single_byte::decode`] with this index.
    SingleByte(&'static single_byte::Index),
    /// [`single_byte::decode_x_user_defined`].
    XUserDefined,
    /// [`utf16::Utf16Decoder`] in this byte order.
    Utf16(utf16::Endian),
    /// [`gb18030::Gb18030Decoder`], for GBK and gb18030.
    Gb18030,
    /// [`big5::Big5Decoder`].
    Big5,
    /// [`euc_jp::EucJpDecoder`].
    EucJp,
    /// [`iso_2022_jp::Iso2022JpDecoder`].
    Iso2022Jp,
    /// [`shift_jis::ShiftJisDecoder`].
    ShiftJis,
    /// [`euc_kr::EucKrDecoder`].
    EucKr,
    /// The replacement encoding (§14.1), which no API decodes with
    /// besides document loading.
    Replacement,
}

impl Encoding {
    /// The decoder of this encoding.
    pub fn family(self) -> Family {
        if let Some(index) = self.single_byte_index() {
            return Family::SingleByte(index);
        }
        match self {
            Encoding::Utf8 => Family::Utf8,
            Encoding::XUserDefined => Family::XUserDefined,
            Encoding::Utf16le => Family::Utf16(utf16::Endian::Little),
            Encoding::Utf16be => Family::Utf16(utf16::Endian::Big),
            Encoding::Gbk | Encoding::Gb18030 => Family::Gb18030,
            Encoding::Big5 => Family::Big5,
            Encoding::EucJp => Family::EucJp,
            Encoding::Iso2022Jp => Family::Iso2022Jp,
            Encoding::ShiftJis => Family::ShiftJis,
            Encoding::EucKr => Family::EucKr,
            Encoding::Replacement => Family::Replacement,
            // single_byte_index answered for every other encoding
            _ => unreachable!("a single-byte encoding without an index"),
        }
    }
}

/// A fatal decoder met input its encoding cannot decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeError;

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("the input is not valid in its encoding")
    }
}

impl core::error::Error for DecodeError {}

/// U+FFFD in UTF-8.
const REPLACEMENT: [u8; 3] = [0xEF, 0xBF, 0xBD];

/// Append the scalar value `c` in UTF-8.
fn push_char(out: &mut Vec<u8>, c: char) {
    out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
}

/// A decoder's error step: an error when `fatal`, else one U+FFFD.
fn error(out: &mut Vec<u8>, fatal: bool) -> Result<(), DecodeError> {
    if fatal {
        return Err(DecodeError);
    }
    out.extend_from_slice(&REPLACEMENT);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_encoding_has_a_family() {
        let multi_byte = (Encoding::ALL.iter())
            .filter(|e| {
                matches!(
                    e.family(),
                    Family::Gb18030
                        | Family::Big5
                        | Family::EucJp
                        | Family::Iso2022Jp
                        | Family::ShiftJis
                        | Family::EucKr
                )
            })
            .count();
        assert_eq!(multi_byte, 7);
        assert!(matches!(Encoding::Gbk.family(), Family::Gb18030));
        assert!(matches!(
            Encoding::Replacement.family(),
            Family::Replacement
        ));
    }
}
