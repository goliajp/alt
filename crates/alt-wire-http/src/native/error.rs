//! What a request of the native transport ends with where it fails.

use alt_remote_protocol::{Failure, RequestKey};

#[derive(Debug)]
pub enum Error {
    Portable(alt_portable::Error),
    Protocol(alt_remote_protocol::Error),
    Invalid(&'static str),
    Limit(&'static str),
    Rejected {
        status: u16,
        failure: Option<Box<Failure>>,
    },
    Unavailable,
    OutcomeUnknown {
        key: Box<RequestKey>,
    },
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Portable(e) => write!(f, "portable protocol: {e}"),
            Self::Protocol(e) => write!(f, "native protocol: {e}"),
            Self::Invalid(what) => write!(f, "invalid native connection or response: {what}"),
            Self::Limit(what) => write!(f, "native transfer limit: {what}"),
            Self::Rejected { status, .. } => write!(f, "native HTTP request failed ({status})"),
            Self::Unavailable => f.write_str("native transport unavailable"),
            Self::OutcomeUnknown { .. } => {
                f.write_str("native write outcome is unknown; query the original request")
            }
            Self::Io(e) => write!(f, "local transfer I/O: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Portable(e) => Some(e),
            Self::Protocol(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::Invalid(_)
            | Self::Limit(_)
            | Self::Rejected { .. }
            | Self::Unavailable
            | Self::OutcomeUnknown { .. } => None,
        }
    }
}

impl From<alt_portable::Error> for Error {
    fn from(e: alt_portable::Error) -> Self {
        Self::Portable(e)
    }
}

impl From<alt_remote_protocol::Error> for Error {
    fn from(e: alt_remote_protocol::Error) -> Self {
        Self::Protocol(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
