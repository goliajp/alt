//! The shared UTF-16 decoder (§14.2.1) behind UTF-16BE and UTF-16LE.
//!
//! The decoder resumes across chunks: a byte of an incomplete code
//! unit and a high surrogate still waiting for its low half stay in
//! [`Utf16Decoder`] until the next chunk, and the last chunk turns
//! whatever is left into one error.

use alloc::vec::Vec;

use crate::{DecodeError, error, push_char};

/// The byte order of the code units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    /// UTF-16LE
    Little,
    /// UTF-16BE
    Big,
}

/// The decoder's state between chunks (its UTF-16 lead byte and lead
/// surrogate).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Utf16Decoder {
    /// The first byte of a code unit whose second byte is yet to come.
    lead_byte: Option<u8>,
    /// A high surrogate whose low surrogate is yet to come.
    lead_surrogate: Option<u16>,
}

impl Utf16Decoder {
    /// The state as four bytes, for a caller that keeps it outside the
    /// decoder: which halves are pending (bit 0 the lead byte, bit 1 the
    /// lead surrogate), the lead byte, the lead surrogate little-endian.
    /// A half that is not pending is 0.
    pub fn to_bytes(self) -> [u8; 4] {
        let [s0, s1] = self.lead_surrogate.unwrap_or(0).to_le_bytes();
        let pending =
            u8::from(self.lead_byte.is_some()) | u8::from(self.lead_surrogate.is_some()) << 1;
        [pending, self.lead_byte.unwrap_or(0), s0, s1]
    }

    /// The state [`Self::to_bytes`] wrote. Bits 2 to 7 of the first byte
    /// are ignored, and so is a byte of a half that is not pending; a
    /// lead surrogate that is not a high surrogate decodes as the
    /// standard's decoder would have left none.
    pub fn from_bytes(b: [u8; 4]) -> Self {
        let surrogate = u16::from_le_bytes([b[2], b[3]]);
        Utf16Decoder {
            lead_byte: (b[0] & 1 != 0).then_some(b[1]),
            lead_surrogate: (b[0] & 2 != 0 && (0xD800..=0xDBFF).contains(&surrogate))
                .then_some(surrogate),
        }
    }

    /// Decode the chunk `input`, appending UTF-8 to `out`; `last` marks
    /// the end of the queue.
    pub fn decode(
        &mut self,
        endian: Endian,
        input: &[u8],
        last: bool,
        fatal: bool,
        out: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        out.reserve(input.len() + input.len() / 2);
        for &b in input {
            let Some(lead) = self.lead_byte.take() else {
                self.lead_byte = Some(b);
                continue;
            };
            let unit = match endian {
                Endian::Little => u16::from_le_bytes([lead, b]),
                Endian::Big => u16::from_be_bytes([lead, b]),
            };
            self.unit(unit, fatal, out)?;
        }
        if last && (self.lead_byte.is_some() || self.lead_surrogate.is_some()) {
            *self = Utf16Decoder::default();
            error(out, fatal)?;
        }
        Ok(())
    }

    /// One code unit.
    fn unit(&mut self, unit: u16, fatal: bool, out: &mut Vec<u8>) -> Result<(), DecodeError> {
        if let Some(high) = self.lead_surrogate.take() {
            if (0xDC00..=0xDFFF).contains(&unit) {
                let cp = 0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
                // SAFETY: a high and a low surrogate spell U+10000 to U+10FFFF
                push_char(out, unsafe { char::from_u32_unchecked(cp) });
                return Ok(());
            }
            // the unit goes back to the queue after the error
            error(out, fatal)?;
        }
        match unit {
            0xD800..=0xDBFF => self.lead_surrogate = Some(unit),
            0xDC00..=0xDFFF => error(out, fatal)?,
            // SAFETY: a code unit outside the surrogates is a scalar value
            _ => push_char(out, unsafe { char::from_u32_unchecked(u32::from(unit)) }),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    fn whole(endian: Endian, input: &[u8], fatal: bool) -> Result<String, DecodeError> {
        let mut out = Vec::new();
        Utf16Decoder::default().decode(endian, input, true, fatal, &mut out)?;
        Ok(String::from_utf8(out).expect("utf-8 out"))
    }

    #[test]
    fn both_byte_orders() {
        let le = [0x41, 0x00, 0xE9, 0x00, 0x3D, 0xD8, 0x00, 0xDE];
        assert_eq!(
            whole(Endian::Little, &le, true),
            Ok("A\u{E9}\u{1F600}".into())
        );
        let be = [0x00, 0x41, 0x4E, 0x2D, 0xD8, 0x3D, 0xDE, 0x00];
        assert_eq!(
            whole(Endian::Big, &be, true),
            Ok("A\u{4E2D}\u{1F600}".into())
        );
        // the decoder itself keeps a BOM
        assert_eq!(
            whole(Endian::Little, &[0xFF, 0xFE], true),
            Ok("\u{FEFF}".into())
        );
    }

    #[test]
    fn lone_surrogates_and_odd_bytes_are_errors() {
        let le = Endian::Little;
        // a lone low, a high before a non-surrogate, a high before a high
        let input = [
            0x00, 0xDC, 0x00, 0xD8, 0x41, 0x00, 0x00, 0xD8, 0x00, 0xD8, 0x00, 0xDC,
        ];
        assert_eq!(
            whole(le, &input, false),
            Ok("\u{FFFD}\u{FFFD}A\u{FFFD}\u{10000}".into())
        );
        assert_eq!(whole(le, &input, true), Err(DecodeError));
        // an odd byte, a high surrogate and an odd byte: one error each end
        assert_eq!(
            whole(le, &[0x41, 0x00, 0x42], false),
            Ok("A\u{FFFD}".into())
        );
        assert_eq!(whole(le, &[0x00, 0xD8, 0x42], false), Ok("\u{FFFD}".into()));
        assert_eq!(whole(le, &[0x00, 0xD8], true), Err(DecodeError));
    }

    #[test]
    fn the_state_round_trips_through_its_bytes() {
        let mut d = Utf16Decoder::default();
        let mut out = Vec::new();
        d.decode(Endian::Big, &[0xD8, 0x3D, 0xDE], false, true, &mut out)
            .expect("decodes");
        assert_eq!(d.to_bytes(), [3, 0xDE, 0x3D, 0xD8]);
        let mut e = Utf16Decoder::from_bytes(d.to_bytes());
        assert_eq!(e, d);
        e.decode(Endian::Big, &[0x00], true, true, &mut out)
            .expect("decodes");
        assert_eq!(out, "\u{1F600}".as_bytes());
        assert_eq!(Utf16Decoder::default().to_bytes(), [0; 4]);
        // a pending half's byte is kept; a lead that is no high surrogate is dropped
        assert_eq!(
            Utf16Decoder::from_bytes([1, 0, 0x41, 0]).to_bytes(),
            [1, 0, 0, 0]
        );
        assert_eq!(
            Utf16Decoder::from_bytes([2, 0, 0x41, 0]),
            Utf16Decoder::default()
        );
    }

    #[test]
    fn a_chunk_boundary_splits_nothing() {
        let le = [0x3D, 0xD8, 0x00, 0xDE, 0x41, 0x00];
        for cut in 0..=le.len() {
            let mut d = Utf16Decoder::default();
            let mut out = Vec::new();
            d.decode(Endian::Little, &le[..cut], false, true, &mut out)
                .expect("decodes");
            d.decode(Endian::Little, &le[cut..], true, true, &mut out)
                .expect("decodes");
            assert_eq!(out, "\u{1F600}A".as_bytes(), "cut at {cut}");
        }
    }
}
