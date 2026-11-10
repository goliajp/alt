//! Explicit, bounded publication packages. This crate never scans `.alt`,
//! performs a network request, changes refs or creates source request history.
mod package;
pub use package::*;
mod selection;
pub use selection::*;
mod received;
pub use received::*;

#[derive(Debug)]
pub enum Error {
    Protocol(alt_remote_protocol::Error),
    Task(alt_task_service::Error),
    Record(alt_task::Error),
    Repository(alt_repository_service::Error),
    Io(std::io::Error),
    Invalid(&'static str),
    Limit(&'static str),
    Json(serde_json::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protocol(e) => std::fmt::Display::fmt(e, f),
            Self::Task(e) => std::fmt::Display::fmt(e, f),
            Self::Record(e) => std::fmt::Display::fmt(e, f),
            Self::Repository(e) => std::fmt::Display::fmt(e, f),
            Self::Io(e) => write!(f, "transfer I/O: {e}"),
            Self::Invalid(what) => write!(f, "invalid transfer package: {what}"),
            Self::Limit(what) => write!(f, "transfer package limit: {what}"),
            Self::Json(e) => write!(f, "transfer encoding: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Protocol(e) => e.source(),
            Self::Task(e) => e.source(),
            Self::Record(e) => e.source(),
            Self::Repository(e) => e.source(),
            Self::Io(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::Invalid(_) | Self::Limit(_) => None,
        }
    }
}

impl From<alt_remote_protocol::Error> for Error {
    fn from(e: alt_remote_protocol::Error) -> Self {
        Self::Protocol(e)
    }
}

impl From<alt_task_service::Error> for Error {
    fn from(e: alt_task_service::Error) -> Self {
        Self::Task(e)
    }
}

impl From<alt_task::Error> for Error {
    fn from(e: alt_task::Error) -> Self {
        Self::Record(e)
    }
}

impl From<alt_repository_service::Error> for Error {
    fn from(e: alt_repository_service::Error) -> Self {
        Self::Repository(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
