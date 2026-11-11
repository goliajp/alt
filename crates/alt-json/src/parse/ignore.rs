//! Skipping a value that nothing will read.
//!
//! Skipping checks structure only: string contents are not validated as
//! UTF-8, surrogate escapes are not paired, numbers are not range-checked
//! and nesting is unlimited. A struct that ignores unknown fields therefore
//! accepts some field values that would be rejected if they were read.

use super::Parser;
use crate::Result;
use crate::error::ErrorCode;
use crate::scan::find_special;

impl Parser<'_> {
    pub(crate) fn ignore_value(&mut self) -> Result<()> {
        // open containers, innermost last
        let mut open: Vec<u8> = Vec::new();
        let mut enclosing: Option<u8> = None;
        loop {
            let Some(peek) = self.skip_whitespace() else {
                return Err(self.peek_error(ErrorCode::EofWhileParsingValue));
            };
            let opened = match peek {
                b'n' => {
                    self.eat();
                    self.parse_ident(b"ull")?;
                    None
                }
                b't' => {
                    self.eat();
                    self.parse_ident(b"rue")?;
                    None
                }
                b'f' => {
                    self.eat();
                    self.parse_ident(b"alse")?;
                    None
                }
                b'-' => {
                    self.eat();
                    self.ignore_integer()?;
                    None
                }
                b'0'..=b'9' => {
                    self.ignore_integer()?;
                    None
                }
                b'"' => {
                    self.eat();
                    self.ignore_str()?;
                    None
                }
                b'[' | b'{' => {
                    open.extend(enclosing.take());
                    self.eat();
                    Some(peek)
                }
                _ => return Err(self.peek_error(ErrorCode::ExpectedSomeValue)),
            };

            let (mut accept_comma, mut frame) = match opened {
                Some(frame) => (false, frame),
                None => match enclosing.take().or_else(|| open.pop()) {
                    Some(frame) => (true, frame),
                    None => return Ok(()),
                },
            };

            // close every container that ends here
            loop {
                match self.skip_whitespace() {
                    Some(b',') if accept_comma => {
                        self.eat();
                        break;
                    }
                    Some(b']') if frame == b'[' => {}
                    Some(b'}') if frame == b'{' => {}
                    Some(_) if accept_comma => {
                        return Err(self.peek_error(if frame == b'[' {
                            ErrorCode::ExpectedListCommaOrEnd
                        } else {
                            ErrorCode::ExpectedObjectCommaOrEnd
                        }));
                    }
                    Some(_) => break,
                    None => {
                        return Err(self.peek_error(if frame == b'[' {
                            ErrorCode::EofWhileParsingList
                        } else {
                            ErrorCode::EofWhileParsingObject
                        }));
                    }
                }
                self.eat();
                frame = match open.pop() {
                    Some(frame) => frame,
                    None => return Ok(()),
                };
                accept_comma = true;
            }

            if frame == b'{' {
                match self.skip_whitespace() {
                    Some(b'"') => self.eat(),
                    Some(_) => return Err(self.peek_error(ErrorCode::KeyMustBeAString)),
                    None => return Err(self.peek_error(ErrorCode::EofWhileParsingObject)),
                }
                self.ignore_str()?;
                match self.skip_whitespace() {
                    Some(b':') => self.eat(),
                    Some(_) => return Err(self.peek_error(ErrorCode::ExpectedColon)),
                    None => return Err(self.peek_error(ErrorCode::EofWhileParsingObject)),
                }
            }
            enclosing = Some(frame);
        }
    }

    fn ignore_str(&mut self) -> Result<()> {
        loop {
            self.index = find_special(self.input, self.index);
            let Some(byte) = self.peek() else {
                return Err(self.error(ErrorCode::EofWhileParsingString));
            };
            match byte {
                b'"' => {
                    self.eat();
                    return Ok(());
                }
                b'\\' => {
                    self.eat();
                    match self.next() {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {}
                        Some(b'u') => {
                            self.parse_hex4()?;
                        }
                        Some(_) => return Err(self.error(ErrorCode::InvalidEscape)),
                        None => return Err(self.error(ErrorCode::EofWhileParsingString)),
                    }
                }
                _ => return Err(self.error(ErrorCode::ControlCharacterWhileParsingString)),
            }
        }
    }

    fn ignore_integer(&mut self) -> Result<()> {
        match self.next().unwrap_or(0) {
            b'0' => {
                if let b'0'..=b'9' = self.peek_or_null() {
                    return Err(self.peek_error(ErrorCode::InvalidNumber));
                }
            }
            b'1'..=b'9' => self.ignore_digits(),
            _ => return Err(self.error(ErrorCode::InvalidNumber)),
        }
        match self.peek_or_null() {
            b'.' => {
                self.eat();
                if !self.peek_or_null().is_ascii_digit() {
                    return Err(self.peek_error(ErrorCode::InvalidNumber));
                }
                self.ignore_digits();
                match self.peek_or_null() {
                    b'e' | b'E' => self.ignore_exponent(),
                    _ => Ok(()),
                }
            }
            b'e' | b'E' => self.ignore_exponent(),
            _ => Ok(()),
        }
    }

    fn ignore_exponent(&mut self) -> Result<()> {
        self.eat();
        if let b'+' | b'-' = self.peek_or_null() {
            self.eat();
        }
        match self.next().unwrap_or(0) {
            b'0'..=b'9' => {}
            _ => return Err(self.error(ErrorCode::InvalidNumber)),
        }
        self.ignore_digits();
        Ok(())
    }

    fn ignore_digits(&mut self) {
        while let b'0'..=b'9' = self.peek_or_null() {
            self.eat();
        }
    }
}
