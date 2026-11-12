//! `application/x-www-form-urlencoded` parsing and serializing.
use crate::percent::{hex_value, push_byte};
use std::borrow::Cow;

/// Iterates over the name-value pairs in `input`.
pub fn parse(input: &[u8]) -> Parse<'_> {
    Parse { input }
}

#[derive(Clone, Copy, Debug)]
pub struct Parse<'a> {
    input: &'a [u8],
}

impl<'a> Iterator for Parse<'a> {
    type Item = (Cow<'a, str>, Cow<'a, str>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.input.is_empty() {
                return None;
            }
            let end = self.input.iter().position(|&b| b == b'&');
            let sequence = &self.input[..end.unwrap_or(self.input.len())];
            self.input = end.map_or(&[], |end| &self.input[end + 1..]);
            if sequence.is_empty() {
                continue;
            }
            let split = sequence.iter().position(|&b| b == b'=');
            let (name, value) = match split {
                Some(split) => (&sequence[..split], &sequence[split + 1..]),
                None => (sequence, &sequence[sequence.len()..]),
            };
            return Some((decode(name), decode(value)));
        }
    }
}

impl<'a> Parse<'a> {
    pub fn into_owned(self) -> impl Iterator<Item = (String, String)> + use<'a> {
        self.map(|(name, value)| (name.into_owned(), value.into_owned()))
    }
}

/// Decodes one name or value: `+` is a space, `%XX` is a byte, and bytes
/// that are not UTF-8 become U+FFFD.
pub fn decode(input: &[u8]) -> Cow<'_, str> {
    let Some(first) = input.iter().position(|&b| matches!(b, b'+' | b'%')) else {
        return String::from_utf8_lossy(input);
    };
    let mut bytes = Vec::with_capacity(input.len());
    bytes.extend_from_slice(&input[..first]);
    let mut i = first;
    while i < input.len() {
        let byte = input[i];
        if byte == b'+' {
            bytes.push(b' ');
        } else if byte == b'%'
            && let Some(&[high, low]) = input.get(i + 1..i + 3).and_then(|s| s.first_chunk())
            && let (Some(high), Some(low)) = (hex_value(high), hex_value(low))
        {
            bytes.push(high << 4 | low);
            i += 2;
        } else {
            bytes.push(byte);
        }
        i += 1;
    }
    match String::from_utf8(bytes) {
        Ok(text) => Cow::Owned(text),
        Err(error) => Cow::Owned(String::from_utf8_lossy(error.as_bytes()).into_owned()),
    }
}

/// Appends `input` to `out` in the form-urlencoded byte serialization.
pub fn encode(out: &mut String, input: &str) {
    for &byte in input.as_bytes() {
        match byte {
            b'*' | b'-' | b'.' | b'_' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => push_byte(out, byte),
        }
    }
}

/// Appends `name=value` to `out`; the caller writes the `&` between pairs.
pub fn append_pair(out: &mut String, name: &str, value: &str) {
    encode(out, name);
    out.push('=');
    encode(out, value);
}
