//! Strict JSON parser over a byte slice.
//!
//! Error positions follow one convention throughout: an error about a byte
//! that was consumed is reported at that byte, and an error about a byte that
//! was only looked at is reported one past it (capped at the end of input).

mod ignore;
mod number;
mod string;

use crate::error::ErrorCode;
use crate::{Error, Map, Result, Value};

pub(crate) use number::Parsed;

/// Arrays and objects may nest 127 deep; the 128th level is rejected.
const DEPTH_LIMIT: u8 = 128;

pub(crate) struct Parser<'a> {
    pub(crate) input: &'a [u8],
    pub(crate) index: usize,
    pub(crate) remaining_depth: u8,
}

pub(crate) fn parse(input: &[u8]) -> Result<Value> {
    let mut parser = Parser::new(input);
    let value = parser.parse_value()?;
    parser.end()?;
    Ok(value)
}

impl<'a> Parser<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Parser {
            input,
            index: 0,
            remaining_depth: DEPTH_LIMIT,
        }
    }

    /// Only whitespace may remain.
    pub(crate) fn end(&mut self) -> Result<()> {
        match self.skip_whitespace() {
            Some(_) => Err(self.peek_error(ErrorCode::TrailingCharacters)),
            None => Ok(()),
        }
    }

    pub(crate) fn peek(&self) -> Option<u8> {
        self.input.get(self.index).copied()
    }

    /// end of input reads as a zero byte, which no caller treats as valid
    pub(crate) fn peek_or_null(&self) -> u8 {
        self.peek().unwrap_or(0)
    }

    pub(crate) fn eat(&mut self) {
        self.index += 1;
    }

    pub(crate) fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.index += 1;
        Some(byte)
    }

    fn position_of(&self, index: usize) -> (usize, usize) {
        let before = &self.input[..index];
        let line_start = before
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |at| at + 1);
        let line = 1 + before[..line_start]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        (line, index - line_start)
    }

    /// error about the byte just consumed
    #[cold]
    pub(crate) fn error(&self, code: ErrorCode) -> Error {
        let (line, column) = self.position_of(self.index);
        Error::new(code, line, column)
    }

    /// error about the byte under the cursor
    #[cold]
    pub(crate) fn peek_error(&self, code: ErrorCode) -> Error {
        let (line, column) = self.position_of(self.input.len().min(self.index + 1));
        Error::new(code, line, column)
    }

    pub(crate) fn skip_whitespace(&mut self) -> Option<u8> {
        loop {
            match self.peek() {
                Some(b' ' | b'\n' | b'\t' | b'\r') => self.eat(),
                other => return other,
            }
        }
    }

    pub(crate) fn parse_ident(&mut self, rest: &[u8]) -> Result<()> {
        for &expected in rest {
            match self.next() {
                None => return Err(self.error(ErrorCode::EofWhileParsingValue)),
                Some(byte) if byte != expected => {
                    return Err(self.error(ErrorCode::ExpectedSomeIdent));
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    pub(crate) fn parse_value(&mut self) -> Result<Value> {
        let Some(peek) = self.skip_whitespace() else {
            return Err(self.peek_error(ErrorCode::EofWhileParsingValue));
        };
        match peek {
            b'n' => {
                self.eat();
                self.parse_ident(b"ull")?;
                Ok(Value::Null)
            }
            b't' => {
                self.eat();
                self.parse_ident(b"rue")?;
                Ok(Value::Bool(true))
            }
            b'f' => {
                self.eat();
                self.parse_ident(b"alse")?;
                Ok(Value::Bool(false))
            }
            b'-' => {
                self.eat();
                self.parse_number(false).map(Parsed::into_value)
            }
            b'0'..=b'9' => self.parse_number(true).map(Parsed::into_value),
            b'"' => {
                self.eat();
                Ok(Value::String(self.parse_str()?.into_owned()))
            }
            b'[' => {
                self.enter()?;
                let array = self.parse_array()?;
                self.remaining_depth += 1;
                Ok(array)
            }
            b'{' => {
                self.enter()?;
                let object = self.parse_object()?;
                self.remaining_depth += 1;
                Ok(object)
            }
            _ => Err(self.peek_error(ErrorCode::ExpectedSomeValue)),
        }
    }

    /// Consumes the opening bracket or brace.
    pub(crate) fn enter(&mut self) -> Result<()> {
        self.remaining_depth -= 1;
        if self.remaining_depth == 0 {
            return Err(self.peek_error(ErrorCode::RecursionLimitExceeded));
        }
        self.eat();
        Ok(())
    }

    fn parse_array(&mut self) -> Result<Value> {
        let mut items = Vec::new();
        loop {
            let Some(peek) = self.skip_whitespace() else {
                return Err(self.peek_error(ErrorCode::EofWhileParsingList));
            };
            if peek == b']' {
                self.eat();
                return Ok(Value::Array(items));
            }
            if !items.is_empty() {
                if peek != b',' {
                    return Err(self.peek_error(ErrorCode::ExpectedListCommaOrEnd));
                }
                self.eat();
                match self.skip_whitespace() {
                    Some(b']') => return Err(self.peek_error(ErrorCode::TrailingComma)),
                    Some(_) => {}
                    None => return Err(self.peek_error(ErrorCode::EofWhileParsingValue)),
                }
            }
            items.push(self.parse_value()?);
        }
    }

    fn parse_object(&mut self) -> Result<Value> {
        let mut map = Map::new();
        let mut first = true;
        loop {
            let Some(peek) = self.skip_whitespace() else {
                return Err(self.peek_error(ErrorCode::EofWhileParsingObject));
            };
            if peek == b'}' {
                self.eat();
                return Ok(Value::Object(map));
            }
            if first {
                first = false;
                if peek != b'"' {
                    return Err(self.peek_error(ErrorCode::KeyMustBeAString));
                }
            } else if peek == b',' {
                self.eat();
                match self.skip_whitespace() {
                    Some(b'"') => {}
                    Some(b'}') => return Err(self.peek_error(ErrorCode::TrailingComma)),
                    Some(_) => return Err(self.peek_error(ErrorCode::KeyMustBeAString)),
                    None => return Err(self.peek_error(ErrorCode::EofWhileParsingValue)),
                }
            } else {
                return Err(self.peek_error(ErrorCode::ExpectedObjectCommaOrEnd));
            }
            self.eat();
            let key = self.parse_str()?.into_owned();
            match self.skip_whitespace() {
                Some(b':') => self.eat(),
                Some(_) => return Err(self.peek_error(ErrorCode::ExpectedColon)),
                None => return Err(self.peek_error(ErrorCode::EofWhileParsingObject)),
            }
            let value = self.parse_value()?;
            // a repeated key keeps the last value
            map.insert(key, value);
        }
    }
}
