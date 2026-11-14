//! Legacy charsets to UTF-8, for commits that declare an `encoding`.

use torajs_encoding::big5::Big5Decoder;
use torajs_encoding::euc_jp::EucJpDecoder;
use torajs_encoding::euc_kr::EucKrDecoder;
use torajs_encoding::gb18030::Gb18030Decoder;
use torajs_encoding::iso_2022_jp::Iso2022JpDecoder;
use torajs_encoding::shift_jis::ShiftJisDecoder;
use torajs_encoding::single_byte;
use torajs_encoding::utf16::{Endian, Utf16Decoder};
use torajs_encoding::{Encoding, Family};

/// `data` converted to UTF-8 from the charset `label` names (a WHATWG
/// encoding label). `None` when the label is unknown or `data` is not valid
/// in that charset.
pub fn decode(label: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    decode_into(label, data, &mut out).then_some(out)
}

/// [`decode`] into a buffer the caller keeps: the decoders reserve room for the
/// longest output an input can have, which is cheaper kept than asked for again
/// for every commit. `out` holds the text when this returns true.
pub fn decode_into(label: &[u8], data: &[u8], out: &mut Vec<u8>) -> bool {
    out.clear();
    let Some(encoding) = Encoding::for_label(label) else {
        return false;
    };
    let mut family = encoding.family();
    let mut data = data;
    // a byte order mark overrides the label
    for (bom, marked) in [
        (&[0xEF, 0xBB, 0xBF][..], Family::Utf8),
        (&[0xFF, 0xFE][..], Family::Utf16(Endian::Little)),
        (&[0xFE, 0xFF][..], Family::Utf16(Endian::Big)),
    ] {
        if let Some(rest) = data.strip_prefix(bom) {
            (family, data) = (marked, rest);
            break;
        }
    }
    let o = out;
    let decoded = match family {
        Family::Utf8 => {
            let Ok(text) = std::str::from_utf8(data) else {
                return false;
            };
            o.extend_from_slice(text.as_bytes());
            Ok(())
        }
        Family::SingleByte(index) => single_byte::decode(index, data, true, o),
        Family::XUserDefined => {
            single_byte::decode_x_user_defined(data, o);
            Ok(())
        }
        Family::Utf16(endian) => Utf16Decoder::default().decode(endian, data, true, true, o),
        Family::Gb18030 => Gb18030Decoder::default().decode(data, true, true, o),
        Family::Big5 => Big5Decoder::default().decode(data, true, true, o),
        Family::EucJp => EucJpDecoder::default().decode(data, true, true, o),
        Family::Iso2022Jp => Iso2022JpDecoder::default().decode(data, true, true, o),
        Family::ShiftJis => ShiftJisDecoder::default().decode(data, true, true, o),
        Family::EucKr => EucKrDecoder::default().decode(data, true, true, o),
        // the replacement encoding decodes nothing but empty input
        _ => return data.is_empty(),
    };
    decoded.is_ok()
}
