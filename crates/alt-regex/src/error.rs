//! Why a pattern is refused, or a search given up.

/// Why a pattern was refused or a search given up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// A parenthesis without its partner.
    Paren,
    /// A bracket expression without its end.
    Bracket,
    /// A malformed `{m,n}`.
    Brace,
    /// A repetition with nothing to repeat.
    Repeat,
    /// A backslash at the end of the pattern.
    Escape,
    /// A range that ends before it begins.
    Range,
    /// An unknown character class name.
    Class,
    /// A collating element of more than one byte.
    Collate,
    /// A back-reference to a group that is not complete where it stands.
    BackReference,
    /// A pattern too large to compile.
    TooBig,
    /// A search that came to the bound on what a search may hold or on
    /// the steps it may take.
    Exhausted,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::Paren => "unmatched parenthesis",
            Error::Bracket => "unterminated bracket expression",
            Error::Brace => "invalid repetition count",
            Error::Repeat => "repetition with nothing to repeat",
            Error::Escape => "trailing backslash",
            Error::Range => "invalid range end",
            Error::Class => "invalid character class name",
            Error::Collate => "invalid collating element",
            Error::BackReference => "invalid back reference",
            Error::TooBig => "regular expression too big",
            Error::Exhausted => "search out of space",
        })
    }
}

impl std::error::Error for Error {}
