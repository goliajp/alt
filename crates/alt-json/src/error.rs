use std::fmt;

/// Why a document was rejected, with the position where parsing stopped.
pub struct Error {
    inner: Box<Inner>,
}

struct Inner {
    code: ErrorCode,
    line: usize,
    column: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorCode {
    EofWhileParsingList,
    EofWhileParsingObject,
    EofWhileParsingString,
    EofWhileParsingValue,
    ExpectedColon,
    ExpectedListCommaOrEnd,
    ExpectedObjectCommaOrEnd,
    ExpectedSomeIdent,
    ExpectedSomeValue,
    InvalidEscape,
    InvalidNumber,
    NumberOutOfRange,
    InvalidUnicodeCodePoint,
    ControlCharacterWhileParsingString,
    KeyMustBeAString,
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
    /// The input is not syntactically valid JSON.
    Syntax,
    /// The input ended before the document was complete.
    Eof,
}

impl Error {
    pub(crate) fn new(code: ErrorCode, line: usize, column: usize) -> Self {
        Self {
            inner: Box::new(Inner { code, line, column }),
        }
    }

    /// One-based line at which the error was detected.
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
            ErrorCode::EofWhileParsingList
            | ErrorCode::EofWhileParsingObject
            | ErrorCode::EofWhileParsingString
            | ErrorCode::EofWhileParsingValue => Category::Eof,
            _ => Category::Syntax,
        }
    }

    pub fn is_syntax(&self) -> bool {
        self.classify() == Category::Syntax
    }

    pub fn is_eof(&self) -> bool {
        self.classify() == Category::Eof
    }
}

impl ErrorCode {
    fn message(self) -> &'static str {
        match self {
            Self::EofWhileParsingList => "EOF while parsing a list",
            Self::EofWhileParsingObject => "EOF while parsing an object",
            Self::EofWhileParsingString => "EOF while parsing a string",
            Self::EofWhileParsingValue => "EOF while parsing a value",
            Self::ExpectedColon => "expected `:`",
            Self::ExpectedListCommaOrEnd => "expected `,` or `]`",
            Self::ExpectedObjectCommaOrEnd => "expected `,` or `}`",
            Self::ExpectedSomeIdent => "expected ident",
            Self::ExpectedSomeValue => "expected value",
            Self::InvalidEscape => "invalid escape",
            Self::InvalidNumber => "invalid number",
            Self::NumberOutOfRange => "number out of range",
            Self::InvalidUnicodeCodePoint => "invalid unicode code point",
            Self::ControlCharacterWhileParsingString => {
                "control character (\\u0000-\\u001F) found while parsing a string"
            }
            Self::KeyMustBeAString => "key must be a string",
            Self::LoneLeadingSurrogateInHexEscape => "lone leading surrogate in hex escape",
            Self::TrailingComma => "trailing comma",
            Self::TrailingCharacters => "trailing characters",
            Self::UnexpectedEndOfHexEscape => "unexpected end of hex escape",
            Self::RecursionLimitExceeded => "recursion limit exceeded",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at line {} column {}",
            self.inner.code.message(),
            self.inner.line,
            self.inner.column
        )
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Error({:?}, line: {}, column: {})",
            self.inner.code.message(),
            self.inner.line,
            self.inner.column
        )
    }
}

impl std::error::Error for Error {}
