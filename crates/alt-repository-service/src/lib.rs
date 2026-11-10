//! Repository publication semantics, independent of HTTP, SQL and local refs.
//!
//! This crate validates a snapshot. The transaction adapter must lock and recheck
//! scope, authorization, policy, refs and object pins before committing it.
mod graph;
mod publication;

pub use graph::*;
pub use publication::*;

#[derive(Debug)]
pub enum Error {
    Protocol(alt_remote_protocol::Error),
    Missing(alt_git_codec::ObjectId),
    Integrity(&'static str),
    Limit(&'static str),
    Precondition(String),
    Forbidden(&'static str),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protocol(e) => write!(f, "invalid publication: {e}"),
            Self::Missing(v) => write!(f, "object unavailable: {v}"),
            Self::Integrity(v) => write!(f, "object integrity failure: {v}"),
            Self::Limit(v) => write!(f, "resource limit exceeded: {v}"),
            Self::Precondition(v) => write!(f, "reference precondition failed: {v}"),
            Self::Forbidden(v) => write!(f, "publication forbidden: {v}"),
            Self::Io(e) => write!(f, "object I/O: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Protocol(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
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
