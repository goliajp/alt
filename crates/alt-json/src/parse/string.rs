use std::borrow::Cow;

use super::Parser;
use crate::Result;
use crate::error::ErrorCode;
use crate::scan::find_special;

impl<'a> Parser<'a> {
    /// Parses the rest of a string whose opening quote has been consumed.
    /// The text is borrowed from the input when it has no escapes.
    ///
    /// Raw bytes are checked as UTF-8 only once the closing quote is reached,
    /// so an earlier syntax error in the same string is reported first.
    pub(crate) fn parse_str(&mut self) -> Result<Cow<'a, str>> {
        let mut unescaped: Vec<u8> = Vec::new();
        let mut start = self.index;
        loop {
            self.index = find_special(self.input, self.index);
            let Some(byte) = self.peek() else {
                return Err(self.error(ErrorCode::EofWhileParsingString));
            };
            let run = &self.input[start..self.index];
            self.eat();
            match byte {
                b'"' => {
                    let text = if unescaped.is_empty() {
                        std::str::from_utf8(run).map(Cow::Borrowed).ok()
                    } else {
                        unescaped.extend_from_slice(run);
                        String::from_utf8(unescaped).map(Cow::Owned).ok()
                    };
                    return text.ok_or_else(|| self.error(ErrorCode::InvalidUnicodeCodePoint));
                }
                b'\\' => {
                    unescaped.extend_from_slice(run);
                    self.parse_escape(&mut unescaped)?;
                    start = self.index;
                }
                _ => return Err(self.error(ErrorCode::ControlCharacterWhileParsingString)),
            }
        }
    }

    /// Parses an escape whose backslash has been consumed.
    fn parse_escape(&mut self, out: &mut Vec<u8>) -> Result<()> {
        let Some(byte) = self.next() else {
            return Err(self.error(ErrorCode::EofWhileParsingString));
        };
        out.push(match byte {
            b'"' => b'"',
            b'\\' => b'\\',
            b'/' => b'/',
            b'b' => 0x08,
            b'f' => 0x0c,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'u' => return self.parse_unicode_escape(out),
            _ => return Err(self.error(ErrorCode::InvalidEscape)),
        });
        Ok(())
    }

    /// Parses the hex digits after `\u`, and the second escape of a
    /// surrogate pair. A surrogate that is not half of a well-formed pair is
    /// an error.
    fn parse_unicode_escape(&mut self, out: &mut Vec<u8>) -> Result<()> {
        let first = self.parse_hex4()?;
        if (0xdc00..=0xdfff).contains(&first) {
            return Err(self.error(ErrorCode::LoneLeadingSurrogateInHexEscape));
        }
        if !(0xd800..=0xdbff).contains(&first) {
            push_code_point(u32::from(first), out);
            return Ok(());
        }
        for expected in *b"\\u" {
            let Some(byte) = self.next() else {
                return Err(self.error(ErrorCode::EofWhileParsingString));
            };
            if byte != expected {
                return Err(self.error(ErrorCode::UnexpectedEndOfHexEscape));
            }
        }
        let second = self.parse_hex4()?;
        if !(0xdc00..=0xdfff).contains(&second) {
            return Err(self.error(ErrorCode::LoneLeadingSurrogateInHexEscape));
        }
        let high = u32::from(first - 0xd800);
        let low = u32::from(second - 0xdc00);
        push_code_point(0x1_0000 + ((high << 10) | low), out);
        Ok(())
    }

    pub(super) fn parse_hex4(&mut self) -> Result<u16> {
        let Some(digits) = self.input.get(self.index..self.index + 4) else {
            self.index = self.input.len();
            return Err(self.error(ErrorCode::EofWhileParsingString));
        };
        self.index += 4;
        let mut value = 0u16;
        for &digit in digits {
            let nibble = match digit {
                b'0'..=b'9' => digit - b'0',
                b'a'..=b'f' => digit - b'a' + 10,
                b'A'..=b'F' => digit - b'A' + 10,
                _ => return Err(self.error(ErrorCode::InvalidEscape)),
            };
            value = (value << 4) | u16::from(nibble);
        }
        Ok(value)
    }
}

/// Appends the UTF-8 encoding of a scalar value.
fn push_code_point(n: u32, out: &mut Vec<u8>) {
    let tail = |shift: u32| 0x80 | ((n >> shift) & 0x3f) as u8;
    match n {
        0..=0x7f => out.push(n as u8),
        0x80..=0x7ff => out.extend_from_slice(&[0xc0 | (n >> 6) as u8, tail(0)]),
        0x800..=0xffff => out.extend_from_slice(&[0xe0 | (n >> 12) as u8, tail(6), tail(0)]),
        _ => out.extend_from_slice(&[0xf0 | (n >> 18) as u8, tail(12), tail(6), tail(0)]),
    }
}
