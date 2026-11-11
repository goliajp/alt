use std::fmt;
use std::io;

/// Why a document was rejected or a value could not be written.
///
/// Syntax errors carry the position where parsing stopped. Errors about the
/// meaning of well-formed input (a wrong type, a missing field) carry the
/// position of the innermost enclosing value that finished parsing, or line 0
/// when there is none.
pub struct Error {
    inner: Box<Inner>,
}

struct Inner {
    code: ErrorCode,
    line: usize,
    column: usize,
}

pub(crate) enum ErrorCode {
    Message(Box<str>),
    Io(io::Error),
    EofWhileParsingList,
    EofWhileParsingObject,
    EofWhileParsingString,
    EofWhileParsingValue,
    ExpectedColon,
    ExpectedListCommaOrEnd,
    ExpectedObjectCommaOrEnd,
    ExpectedSomeIdent,
    ExpectedSomeValue,
    ExpectedDoubleQuote,
    InvalidEscape,
    InvalidNumber,
    NumberOutOfRange,
    InvalidUnicodeCodePoint,
    ControlCharacterWhileParsingString,
    KeyMustBeAString,
    ExpectedNumericKey,
    LoneLeadingSurrogateInHexEscape,
    TrailingComma,
    TrailingCharacters,
    UnexpectedEndOfHexEscape,
    RecursionLimitExceeded,
}

/// Broad class of an [`Error`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Category {
    /// Reading or writing the underlying stream failed.
    Io,
    /// The input is not syntactically valid JSON.
    Syntax,
    /// The input is valid JSON but does not fit the requested type.
    Data,
    /// The input ended before the document was complete.
    Eof,
}

impl Error {
    pub(crate) fn new(code: ErrorCode, line: usize, column: usize) -> Self {
        Self {
            inner: Box::new(Inner { code, line, column }),
        }
    }

    pub(crate) fn io(error: io::Error) -> Self {
        Self::new(ErrorCode::Io(error), 0, 0)
    }

    /// One-based line at which the error was detected, or 0 when unknown.
    pub fn line(&self) -> usize {
        self.inner.line
    }

    /// Column at which the error was detected; the first byte of a line is
    /// column 1, and an error right after a newline is reported in column 0.
    pub fn column(&self) -> usize {
        self.inner.column
    }

    pub fn classify(&self) -> Category {
        match self.inner.code {
            ErrorCode::Message(_) => Category::Data,
            ErrorCode::Io(_) => Category::Io,
            ErrorCode::EofWhileParsingList
            | ErrorCode::EofWhileParsingObject
            | ErrorCode::EofWhileParsingString
            | ErrorCode::EofWhileParsingValue => Category::Eof,
            _ => Category::Syntax,
        }
    }

    pub fn is_io(&self) -> bool {
        self.classify() == Category::Io
    }

    pub fn is_syntax(&self) -> bool {
        self.classify() == Category::Syntax
    }

    pub fn is_data(&self) -> bool {
        self.classify() == Category::Data
    }

    pub fn is_eof(&self) -> bool {
        self.classify() == Category::Eof
    }

    pub(crate) fn has_position(&self) -> bool {
        self.inner.line != 0
    }

    /// Moves the error out of an unpositioned one so it can be given a
    /// position.
    pub(crate) fn into_code(self) -> ErrorCode {
        self.inner.code
    }

    /// Drops the position.
    pub(crate) fn unpositioned(mut self) -> Self {
        self.inner.line = 0;
        self.inner.column = 0;
        self
    }

    /// An error with a free-form message, for conversions that validate.
    pub fn custom(message: impl fmt::Display) -> Self {
        Self::new(ErrorCode::Message(message.to_string().into()), 0, 0)
    }

    /// `unexpected` describes what was found, `expected` what the type needs.
    pub fn invalid_type(unexpected: impl fmt::Display, expected: impl fmt::Display) -> Self {
        Self::custom(format_args!(
            "invalid type: {unexpected}, expected {expected}"
        ))
    }

    pub fn invalid_value(unexpected: impl fmt::Display, expected: impl fmt::Display) -> Self {
        Self::custom(format_args!(
            "invalid value: {unexpected}, expected {expected}"
        ))
    }

    pub fn invalid_length(length: usize, expected: impl fmt::Display) -> Self {
        Self::custom(format_args!("invalid length {length}, expected {expected}"))
    }

    pub fn unknown_field(field: &str, expected: &[&str]) -> Self {
        Self::custom(Unknown("field", field, expected))
    }

    pub fn unknown_variant(variant: &str, expected: &[&str]) -> Self {
        Self::custom(Unknown("variant", variant, expected))
    }

    pub fn missing_field(field: &str) -> Self {
        Self::custom(format_args!("missing field `{field}`"))
    }

    pub fn duplicate_field(field: &str) -> Self {
        Self::custom(format_args!("duplicate field `{field}`"))
    }

    /// The error for an object key that is not a string, integer or name.
    pub fn key_must_be_a_string() -> Self {
        Self::new(ErrorCode::KeyMustBeAString, 0, 0)
    }
}

struct Unknown<'a>(&'a str, &'a str, &'a [&'a str]);

impl fmt::Display for Unknown<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Unknown(kind, name, expected) = self;
        write!(f, "unknown {kind} `{name}`, ")?;
        match expected {
            [] => write!(f, "there are no {kind}s"),
            [only] => write!(f, "expected `{only}`"),
            [first, second] => write!(f, "expected `{first}` or `{second}`"),
            many => {
                f.write_str("expected one of ")?;
                for (i, name) in many.iter().enumerate() {
                    let separator = if i == 0 { "" } else { ", " };
                    write!(f, "{separator}`{name}`")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Message(message) => message,
            Self::Io(error) => return fmt::Display::fmt(error, f),
            Self::EofWhileParsingList => "EOF while parsing a list",
            Self::EofWhileParsingObject => "EOF while parsing an object",
            Self::EofWhileParsingString => "EOF while parsing a string",
            Self::EofWhileParsingValue => "EOF while parsing a value",
            Self::ExpectedColon => "expected `:`",
            Self::ExpectedListCommaOrEnd => "expected `,` or `]`",
            Self::ExpectedObjectCommaOrEnd => "expected `,` or `}`",
            Self::ExpectedSomeIdent => "expected ident",
            Self::ExpectedSomeValue => "expected value",
            Self::ExpectedDoubleQuote => "expected `\"`",
            Self::InvalidEscape => "invalid escape",
            Self::InvalidNumber => "invalid number",
            Self::NumberOutOfRange => "number out of range",
            Self::InvalidUnicodeCodePoint => "invalid unicode code point",
            Self::ControlCharacterWhileParsingString => {
                "control character (\\u0000-\\u001F) found while parsing a string"
            }
            Self::KeyMustBeAString => "key must be a string",
            Self::ExpectedNumericKey => "invalid value: expected key to be a number in quotes",
            Self::LoneLeadingSurrogateInHexEscape => "lone leading surrogate in hex escape",
            Self::TrailingComma => "trailing comma",
            Self::TrailingCharacters => "trailing characters",
            Self::UnexpectedEndOfHexEscape => "unexpected end of hex escape",
            Self::RecursionLimitExceeded => "recursion limit exceeded",
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Inner { code, line, column } = &*self.inner;
        if *line == 0 {
            fmt::Display::fmt(code, f)
        } else {
            write!(f, "{code} at line {line} column {column}")
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Inner { code, line, column } = &*self.inner;
        write!(
            f,
            "Error({:?}, line: {line}, column: {column})",
            code.to_string()
        )
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.inner.code {
            ErrorCode::Io(error) => error.source(),
            _ => None,
        }
    }
}
