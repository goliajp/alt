//! Percent-encode sets, percent-encoding and percent-decoding.
use std::borrow::Cow;

/// A set of ASCII bytes to percent-encode; every byte above 0x7F is always in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeSet(u128);

impl EncodeSet {
    pub const C0_CONTROL: Self = Self(((1 << 0x20) - 1) | (1 << 0x7f));
    pub const FRAGMENT: Self = Self::C0_CONTROL.with(b" \"<>`");
    pub const QUERY: Self = Self::C0_CONTROL.with(b" \"#<>");
    pub const SPECIAL_QUERY: Self = Self::QUERY.with(b"'");
    pub const PATH: Self = Self::QUERY.with(b"?^`{}");
    pub const USERINFO: Self = Self::PATH.with(b"/:;=@[\\]|");
    pub const COMPONENT: Self = Self::USERINFO.with(b"$%&+,");

    pub const fn with(self, ascii: &[u8]) -> Self {
        let mut bits = self.0;
        let mut i = 0;
        while i < ascii.len() {
            bits |= 1 << ascii[i];
            i += 1;
        }
        Self(bits)
    }

    #[inline]
    pub const fn contains(self, byte: u8) -> bool {
        byte >= 0x80 || (self.0 >> byte) & 1 != 0
    }
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

#[inline]
pub(crate) fn push_byte(out: &mut String, byte: u8) {
    out.push('%');
    out.push(HEX[usize::from(byte >> 4)] as char);
    out.push(HEX[usize::from(byte & 15)] as char);
}

/// Appends `input` to `out`, percent-encoding the bytes in `set`.
pub fn encode(out: &mut String, input: &str, set: EncodeSet) {
    let mut start = 0;
    for (i, &byte) in input.as_bytes().iter().enumerate() {
        if set.contains(byte) {
            // a non-empty run starts and ends next to unencoded ASCII, so on char boundaries
            if start < i {
                out.push_str(&input[start..i]);
            }
            push_byte(out, byte);
            start = i + 1;
        }
    }
    if start < input.len() {
        out.push_str(&input[start..]);
    }
}

#[inline]
pub(crate) fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn decode_into(out: &mut Vec<u8>, input: &[u8]) {
    let mut i = 0;
    while i < input.len() {
        let byte = input[i];
        if byte == b'%'
            && let Some(&[high, low]) = input.get(i + 1..i + 3).and_then(|s| s.first_chunk())
            && let (Some(high), Some(low)) = (hex_value(high), hex_value(low))
        {
            out.push(high << 4 | low);
            i += 3;
        } else {
            out.push(byte);
            i += 1;
        }
    }
}

/// Percent-decodes `input`; a `%` not followed by two hex digits is kept.
pub fn decode(input: &[u8]) -> Cow<'_, [u8]> {
    if !input.contains(&b'%') {
        return Cow::Borrowed(input);
    }
    let mut out = Vec::with_capacity(input.len());
    decode_into(&mut out, input);
    Cow::Owned(out)
}
