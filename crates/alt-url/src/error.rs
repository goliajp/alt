use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseError {
    EmptyHost,
    IdnaError,
    InvalidPort,
    InvalidIpv4Address,
    InvalidIpv6Address,
    InvalidDomainCharacter,
    RelativeUrlWithoutBase,
    RelativeUrlWithCannotBeABaseBase,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptyHost => "empty host",
            Self::IdnaError => "invalid international domain name",
            Self::InvalidPort => "invalid port number",
            Self::InvalidIpv4Address => "invalid IPv4 address",
            Self::InvalidIpv6Address => "invalid IPv6 address",
            Self::InvalidDomainCharacter => "invalid domain character",
            Self::RelativeUrlWithoutBase => "relative URL without a base",
            Self::RelativeUrlWithCannotBeABaseBase => "relative URL with a cannot-be-a-base base",
        })
    }
}

impl std::error::Error for ParseError {}
