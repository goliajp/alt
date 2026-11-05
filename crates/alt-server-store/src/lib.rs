//! PostgreSQL publication authority and a durable immutable POSIX byte store.
mod bytes;
mod gc;
mod publication;
mod read;
mod requests;
pub use publication::*;
pub use read::*;
mod schema;
mod store;
mod tasks;
pub use bytes::*;
pub use store::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("storage I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("database: {0}")]
    Database(#[from] postgres::Error),
    #[error("protocol: {0}")]
    Protocol(#[from] alt_remote_protocol::Error),
    #[error("task: {0}")]
    Task(#[from] alt_task_service::Error),
    #[error("repository: {0}")]
    Repository(#[from] alt_repository_service::Error),
    #[error("encoding: {0}")]
    Json(#[from] serde_json::Error),
    #[error("integrity failure: {0}")]
    Integrity(&'static str),
    #[error("storage limit exceeded: {0}")]
    Limit(&'static str),
    #[error("precondition failed: {0}")]
    Precondition(&'static str),
    #[error("access denied")]
    Unauthorized,
    #[error("transaction outcome unknown; query the original request")]
    OutcomeUnknown,
}
pub type Result<T> = std::result::Result<T, Error>;

mod transport;
pub use transport::*;

mod chunks;
pub use chunks::*;
