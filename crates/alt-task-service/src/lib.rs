//! Validation of explicitly selected immutable task history. Acceptance is a
//! separate, current server decision; imported records never imply execution
//! by this instance or grant permission to mutate a ref.
mod records;
mod strict_json;
pub use records::*;

#[derive(Debug)]
pub enum Error {
    Repository(alt_repository_service::Error),
    Integrity(&'static str),
    Limit(&'static str),
    Json(alt_json::Error),
    Io(std::io::Error),
    Task(alt_task::Error),
    Input(alt_input::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Repository(e) => std::fmt::Display::fmt(e, f),
            Self::Integrity(v) => write!(f, "task graph integrity: {v}"),
            Self::Limit(v) => write!(f, "task graph limit: {v}"),
            Self::Json(e) => write!(f, "task graph JSON: {e}"),
            Self::Io(e) => write!(f, "task graph I/O: {e}"),
            Self::Task(e) => std::fmt::Display::fmt(e, f),
            Self::Input(e) => std::fmt::Display::fmt(e, f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Repository(e) => e.source(),
            Self::Json(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::Task(e) => e.source(),
            Self::Input(e) => e.source(),
            _ => None,
        }
    }
}

impl From<alt_repository_service::Error> for Error {
    fn from(e: alt_repository_service::Error) -> Self {
        Self::Repository(e)
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

impl From<alt_task::Error> for Error {
    fn from(e: alt_task::Error) -> Self {
        Self::Task(e)
    }
}

impl From<alt_input::Error> for Error {
    fn from(e: alt_input::Error) -> Self {
        Self::Input(e)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

mod source;
pub use source::*;
mod bundle;
pub use bundle::*;

mod graph;
pub use graph::*;

mod inputs;
pub use inputs::verify_candidate_inputs;

mod facts;
pub use facts::*;

mod links;
mod snapshot;
pub use snapshot::verify_baseline_snapshot;
mod trust;
pub use trust::*;
mod review;
pub use review::verify_review_changes;
mod acceptance;
mod content;
pub use acceptance::*;
mod delivery;
pub use delivery::*;
mod source_operation;
pub use source_operation::*;

mod recovery;
pub use recovery::*;

mod verified;
pub use verified::VerifiedTaskGraph;

mod publication;
pub use publication::VerifiedPublication;
