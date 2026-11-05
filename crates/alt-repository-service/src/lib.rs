//! Repository publication semantics, independent of HTTP, SQL and local refs.
//!
//! This crate validates a snapshot. The transaction adapter must lock and recheck
//! scope, authorization, policy, refs and object pins before committing it.
mod graph;
mod publication;

pub use graph::*;
pub use publication::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid publication: {0}")]
    Protocol(#[from] alt_remote_protocol::Error),
    #[error("object unavailable: {0}")]
    Missing(alt_git_codec::ObjectId),
    #[error("object integrity failure: {0}")]
    Integrity(&'static str),
    #[error("resource limit exceeded: {0}")]
    Limit(&'static str),
    #[error("reference precondition failed: {0}")]
    Precondition(String),
    #[error("publication forbidden: {0}")]
    Forbidden(&'static str),
    #[error("object I/O: {0}")]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
