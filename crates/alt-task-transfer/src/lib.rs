//! Explicit, bounded publication packages. This crate never scans `.alt`,
//! performs a network request, changes refs or creates source request history.
mod package;
pub use package::*;
mod selection;
pub use selection::*;
mod received;
pub use received::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Protocol(#[from] alt_remote_protocol::Error),
    #[error(transparent)]
    Task(#[from] alt_task_service::Error),
    #[error(transparent)]
    Record(#[from] alt_task::Error),
    #[error(transparent)]
    Repository(#[from] alt_repository_service::Error),
    #[error("transfer I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid transfer package: {0}")]
    Invalid(&'static str),
    #[error("transfer package limit: {0}")]
    Limit(&'static str),
    #[error("transfer encoding: {0}")]
    Json(#[from] serde_json::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
