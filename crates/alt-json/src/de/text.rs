//! Typed reads from text.
//!
//! Each read reports a mismatch between the input and the requested type as
//! an unpositioned error and then gives it the position of the cursor, which
//! by then is past the offending scalar, or still in front of an offending
//! array or object.

use std::borrow::Cow;

use crate::error::ErrorCode;
use crate::parse::{Parsed, Parser};
use crate::{Error, Number, Result, Value};

/// How an enum or struct presents itself in the input.
pub(super) enum Shape {
    Seq,
    Map,
    /// a bare string naming a variant
    Name,
}

impl<'a> Parser<'a> {
    pub(super) fn fix(&self, error: Error) -> Error {
        if error.has_position() {
            error
        } else {
            self.error(error.into_code())
        }
    }

    pub(super) fn peek_value(&mut self) -> Result<u8> {
        self.skip_whitespace()
            .ok_or_else(|| self.peek_error(ErrorCode::EofWhileParsingValue))
    }

    /// Describes the value under the cursor in an error, consuming it when
    /// it is a scalar; a malformed scalar is reported as such instead.
    #[cold]
    fn invalid_type(&mut self, expected: &str) -> Error {
        let found = match self.peek_or_null() {
            b'n' => {
                self.eat();
                self.parse_ident(b"ull").map(|()| "null".to_owned())
            }
            b't' => {
                self.eat();
                self.parse_ident(b"rue")
                    .map(|()| "boolean `true`".to_owned())
            }
            b'f' => {
                self.eat();
                self.parse_ident(b"alse")
                    .map(|()| "boolean `false`".to_owned())
            }
            b'-' => {
                self.eat();
                self.parse_number(false).map(describe_number)
            }
            b'0'..=b'9' => self.parse_number(true).map(describe_number),
            b'"' => {
                self.eat();
                self.parse_str().map(|s| format!("string {s:?}"))
            }
            b'[' => Ok("sequence".to_owned()),
            b'{' => Ok("map".to_owned()),
            _ => Err(self.peek_error(ErrorCode::ExpectedSomeValue)),
        };
        match found {
            Ok(found) => self.fix(Error::invalid_type(found, expected)),
            Err(error) => error,
        }
    }

    pub(super) fn read_bool(&mut self) -> Result<bool> {
        match self.peek_value()? {
            b't' => {
                self.eat();
                self.parse_ident(b"rue")?;
                Ok(true)
            }
            b'f' => {
                self.eat();
                self.parse_ident(b"alse")?;
                Ok(false)
            }
            _ => Err(self.invalid_type("a boolean")),
        }
    }

    pub(super) fn read_number(&mut self, expected: &str) -> Result<Number> {
        let parsed = match self.peek_value()? {
            b'-' => {
                self.eat();
                self.parse_number(false)?
            }
            b'0'..=b'9' => self.parse_number(true)?,
            _ => return Err(self.invalid_type(expected)),
        };
        Ok(Number::from_parsed(parsed))
    }

    /// Plain digits only: a fraction or exponent is left unread, and a
    /// non-number is reported as a malformed number.
    pub(super) fn read_integer128(&mut self, signed: bool) -> Result<String> {
        let mut text = String::new();
        if self.peek_value()? == b'-' {
            if !signed {
                return Err(self.peek_error(ErrorCode::NumberOutOfRange));
            }
            self.eat();
            text.push('-');
        }
        match self.next().unwrap_or(0) {
            b'0' => {
                text.push('0');
                if self.peek_or_null().is_ascii_digit() {
                    return Err(self.peek_error(ErrorCode::InvalidNumber));
                }
            }
            first @ b'1'..=b'9' => {
                text.push(char::from(first));
                while let digit @ b'0'..=b'9' = self.peek_or_null() {
                    self.eat();
                    text.push(char::from(digit));
                }
            }
            _ => return Err(self.error(ErrorCode::InvalidNumber)),
        }
        Ok(text)
    }

    pub(super) fn out_of_range(&self) -> Error {
        self.error(ErrorCode::NumberOutOfRange)
    }

    pub(super) fn read_str(&mut self, expected: &str) -> Result<Cow<'a, str>> {
        match self.peek_value()? {
            b'"' => {
                self.eat();
                self.parse_str()
            }
            _ => Err(self.invalid_type(expected)),
        }
    }

    /// Consumes `null` if that is the next value.
    pub(super) fn read_null(&mut self) -> Result<bool> {
        match self.skip_whitespace() {
            Some(b'n') => {
                self.eat();
                self.parse_ident(b"ull")?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub(super) fn read_unit(&mut self, expected: &str) -> Result<()> {
        match self.peek_value()? {
            b'n' => {
                self.eat();
                self.parse_ident(b"ull")
            }
            _ => Err(self.invalid_type(expected)),
        }
    }

    pub(super) fn read_value(&mut self) -> Result<Value> {
        self.parse_value()
    }

    /// Enters an array or object, whichever `accept` allows.
    pub(super) fn begin(&mut self, accept: &[u8], expected: &str) -> Result<Shape> {
        let peek = self.peek_value()?;
        if !accept.contains(&peek) {
            return Err(self.invalid_type(expected));
        }
        self.enter()?;
        Ok(if peek == b'[' { Shape::Seq } else { Shape::Map })
    }

    /// Refuses an array or object after entering it, for readers that
    /// look at any value before deciding. Compared with [`Parser::begin`]
    /// this moves the reported position past the opening bracket, and past
    /// the closing one if the container is empty.
    pub(super) fn refuse_entered(&mut self, refused: &[u8], expected: &str) -> Result<()> {
        let peek = self.peek_value()?;
        if !refused.contains(&peek) {
            return Ok(());
        }
        self.enter()?;
        if peek == b'[' {
            self.finish_seq(Err(Error::invalid_type("sequence", expected)))
        } else {
            self.finish_map(Err(Error::invalid_type("map", expected)))
        }
    }

    /// Leaves an array. A failure inside wins over a failure to close, but
    /// the closing bracket is still consumed first so the position reported
    /// for an unpositioned failure is the same either way.
    pub(super) fn finish_seq<T>(&mut self, inner: Result<T>) -> Result<T> {
        self.remaining_depth += 1;
        let end = self.end_seq();
        inner
            .and_then(|value| end.map(|()| value))
            .map_err(|e| self.fix(e))
    }

    pub(super) fn finish_map<T>(&mut self, inner: Result<T>) -> Result<T> {
        self.remaining_depth += 1;
        let end = self.end_map();
        inner
            .and_then(|value| end.map(|()| value))
            .map_err(|e| self.fix(e))
    }

    fn end_seq(&mut self) -> Result<()> {
        match self.skip_whitespace() {
            Some(b']') => {
                self.eat();
                Ok(())
            }
            Some(b',') => {
                self.eat();
                match self.skip_whitespace() {
                    Some(b']') => Err(self.peek_error(ErrorCode::TrailingComma)),
                    _ => Err(self.peek_error(ErrorCode::TrailingCharacters)),
                }
            }
            Some(_) => Err(self.peek_error(ErrorCode::TrailingCharacters)),
            None => Err(self.peek_error(ErrorCode::EofWhileParsingList)),
        }
    }

    fn end_map(&mut self) -> Result<()> {
        match self.skip_whitespace() {
            Some(b'}') => {
                self.eat();
                Ok(())
            }
            Some(b',') => Err(self.peek_error(ErrorCode::TrailingComma)),
            Some(_) => Err(self.peek_error(ErrorCode::TrailingCharacters)),
            None => Err(self.peek_error(ErrorCode::EofWhileParsingObject)),
        }
    }

    pub(super) fn has_next_element(&mut self, first: &mut bool) -> Result<bool> {
        let Some(peek) = self.skip_whitespace() else {
            return Err(self.peek_error(ErrorCode::EofWhileParsingList));
        };
        if peek == b']' {
            Ok(false)
        } else if *first {
            *first = false;
            Ok(true)
        } else if peek == b',' {
            self.eat();
            match self.skip_whitespace() {
                Some(b']') => Err(self.peek_error(ErrorCode::TrailingComma)),
                Some(_) => Ok(true),
                None => Err(self.peek_error(ErrorCode::EofWhileParsingValue)),
            }
        } else {
            Err(self.peek_error(ErrorCode::ExpectedListCommaOrEnd))
        }
    }

    /// On success the cursor is on the opening quote of the next key.
    pub(super) fn has_next_key(&mut self, first: &mut bool) -> Result<bool> {
        let Some(peek) = self.skip_whitespace() else {
            return Err(self.peek_error(ErrorCode::EofWhileParsingObject));
        };
        if peek == b'}' {
            Ok(false)
        } else if *first {
            *first = false;
            if peek == b'"' {
                Ok(true)
            } else {
                Err(self.peek_error(ErrorCode::KeyMustBeAString))
            }
        } else if peek == b',' {
            self.eat();
            match self.skip_whitespace() {
                Some(b'"') => Ok(true),
                Some(b'}') => Err(self.peek_error(ErrorCode::TrailingComma)),
                Some(_) => Err(self.peek_error(ErrorCode::KeyMustBeAString)),
                None => Err(self.peek_error(ErrorCode::EofWhileParsingValue)),
            }
        } else {
            Err(self.peek_error(ErrorCode::ExpectedObjectCommaOrEnd))
        }
    }

    pub(super) fn object_colon(&mut self) -> Result<()> {
        match self.skip_whitespace() {
            Some(b':') => {
                self.eat();
                Ok(())
            }
            Some(_) => Err(self.peek_error(ErrorCode::ExpectedColon)),
            None => Err(self.peek_error(ErrorCode::EofWhileParsingObject)),
        }
    }

    /// A number written inside the quotes of a key, with nothing else
    /// between them. The cursor is on the opening quote.
    pub(super) fn read_key_number(&mut self, expected: &str) -> Result<Number> {
        self.eat();
        if !matches!(self.peek(), Some(b'0'..=b'9' | b'-')) {
            return Err(self.error(ErrorCode::ExpectedNumericKey));
        }
        self.read_number(expected)
    }

    pub(super) fn end_key_number(&mut self) -> Result<()> {
        match self.peek() {
            Some(b'"') => {
                self.eat();
                Ok(())
            }
            _ => Err(self.peek_error(ErrorCode::ExpectedDoubleQuote)),
        }
    }

    /// An enum is a bare variant name, or an object with one member whose
    /// key names the variant.
    pub(super) fn begin_enum(&mut self) -> Result<Shape> {
        match self.skip_whitespace() {
            Some(b'{') => {
                self.enter()?;
                match self.skip_whitespace() {
                    Some(b'"') => Ok(Shape::Map),
                    Some(b'}') => Err(self.peek_error(ErrorCode::ExpectedSomeValue)),
                    Some(_) => Err(self.peek_error(ErrorCode::KeyMustBeAString)),
                    None => Err(self.peek_error(ErrorCode::EofWhileParsingObject)),
                }
            }
            Some(b'"') => Ok(Shape::Name),
            Some(_) => Err(self.peek_error(ErrorCode::ExpectedSomeValue)),
            None => Err(self.peek_error(ErrorCode::EofWhileParsingValue)),
        }
    }

    /// Whether the value under the cursor can be an enum, judged as a
    /// tree would be: a string, or an object with exactly one member. The
    /// value must be well formed; the cursor does not move.
    pub(super) fn check_enum_shape(&mut self) -> Result<()> {
        let start = self.index;
        let verdict = match self.peek_value()? {
            b'"' => Ok(()),
            b'{' => {
                self.eat();
                let (mut first, mut members) = (true, 0);
                while self.has_next_key(&mut first)? {
                    self.eat();
                    self.parse_str()?;
                    self.object_colon()?;
                    self.ignore_value()?;
                    members += 1;
                }
                if members == 1 {
                    Ok(())
                } else {
                    Err(Error::invalid_value("map", "map with a single key"))
                }
            }
            _ => Err(self.invalid_type("string or map").unpositioned()),
        };
        self.index = start;
        verdict
    }

    pub(super) fn finish_enum<T>(&mut self, inner: Result<T>) -> Result<T> {
        self.remaining_depth += 1;
        let value = inner?;
        match self.skip_whitespace() {
            Some(b'}') => {
                self.eat();
                Ok(value)
            }
            Some(_) => Err(self.error(ErrorCode::ExpectedSomeValue)),
            None => Err(self.error(ErrorCode::EofWhileParsingObject)),
        }
    }
}

fn describe_number(parsed: Parsed) -> String {
    match parsed {
        Parsed::U64(n) => format!("integer `{n}`"),
        Parsed::I64(n) => format!("integer `{n}`"),
        // written as the number is written into JSON, not as Rust shows it
        Parsed::F64(_) => format!("floating point `{}`", Number::from_parsed(parsed)),
    }
}
