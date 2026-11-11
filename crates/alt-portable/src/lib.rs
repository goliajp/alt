//! Portable data contains historical identities, never active credentials.
//! An authenticated catalog and intact archive are prerequisites for semantic
//! object/task validation; they do not authorize a destination transaction.
mod archive;
mod catalog;
mod envelope;
pub mod http;
mod import;
mod semantic;
pub use archive::*;
pub use catalog::*;
pub use envelope::*;
pub use import::*;
pub use semantic::*;

#[derive(Debug)]
pub enum Error {
    Repository(alt_repository_service::Error),
    Task(alt_task_service::Error),
    Protocol(alt_remote_protocol::Error),
    Provenance(alt_provenance::Error),
    Json(alt_json::Error),
    Io(std::io::Error),
    Invalid(&'static str),
    Limit(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Repository(e) => write!(f, "portable object graph: {e}"),
            Self::Task(e) => write!(f, "portable task graph: {e}"),
            Self::Protocol(e) => write!(f, "portable protocol: {e}"),
            Self::Provenance(e) => write!(f, "portable provenance: {e}"),
            Self::Json(e) => write!(f, "portable JSON: {e}"),
            Self::Io(e) => write!(f, "portable I/O: {e}"),
            Self::Invalid(v) => write!(f, "invalid portable package: {v}"),
            Self::Limit(v) => write!(f, "portable limit: {v}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Repository(e) => Some(e),
            Self::Task(e) => Some(e),
            Self::Protocol(e) => Some(e),
            Self::Provenance(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<alt_repository_service::Error> for Error {
    fn from(e: alt_repository_service::Error) -> Self {
        Self::Repository(e)
    }
}

impl From<alt_task_service::Error> for Error {
    fn from(e: alt_task_service::Error) -> Self {
        Self::Task(e)
    }
}

impl From<alt_remote_protocol::Error> for Error {
    fn from(e: alt_remote_protocol::Error) -> Self {
        Self::Protocol(e)
    }
}

impl From<alt_provenance::Error> for Error {
    fn from(e: alt_provenance::Error) -> Self {
        Self::Provenance(e)
    }
}

impl From<alt_json::Error> for Error {
    fn from(e: alt_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
pub type Result<T> = std::result::Result<T, Error>;
