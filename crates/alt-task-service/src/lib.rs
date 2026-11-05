//! Validation of explicitly selected immutable task history. Acceptance is a
//! separate, current server decision; imported records never imply execution
//! by this instance or grant permission to mutate a ref.
mod records;
mod strict_json;
pub use records::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Repository(#[from] alt_repository_service::Error),
    #[error("task graph integrity: {0}")]
    Integrity(&'static str),
    #[error("task graph limit: {0}")]
    Limit(&'static str),
    #[error("task graph JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("task graph I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Task(#[from] alt_task::Error),
    #[error(transparent)]
    Input(#[from] alt_input::Error),
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
