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
    let mut family = Encoding::for_label(label)?.family();
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
    let mut out = Vec::new();
    let o = &mut out;
    let decoded = match family {
        Family::Utf8 => {
            o.extend_from_slice(std::str::from_utf8(data).ok()?.as_bytes());
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
        _ => return data.is_empty().then_some(out),
    };
    decoded.ok().map(|()| out)
}
