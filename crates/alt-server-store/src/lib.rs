//! PostgreSQL publication authority and a durable immutable POSIX byte store.
mod bytes;
mod database;
mod gc;
mod portable;
mod portable_import;
pub use portable::*;
pub use portable_import::*;
mod provenance;
mod publication;
mod read;
mod registration;
mod requests;
pub use publication::*;
pub use read::*;
pub use registration::MAX_REGISTER_CONTENTS;
mod schema;
mod segments;
mod store;
mod tasks;
pub use bytes::*;
pub use store::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("portable migration: {0}")]
    Portable(#[from] alt_portable::Error),
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
    #[error("current account authority is temporarily unavailable")]
    AuthorityUnavailable,
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

mod browser;
pub use browser::*;
mod workbench;
pub use tasks::CurrentReviewAssessment;
pub use workbench::*;

/// Current persisted schema; runtime open validates all migration checksums.
pub const SCHEMA_VERSION: u32 = 17;

mod change_channel;
mod link_history;
pub use change_channel::*;
mod change_delivery;
mod change_events;
mod change_repair;
pub use change_events::*;

mod accounts;
pub use accounts::*;
mod maintenance;
pub use maintenance::*;

mod cloud;
pub use cloud::*;

mod cloud_quota;

mod delegation;

mod cloud_browser;

mod cloud_login;
pub use cloud_login::CloudLoginClaim;

mod device_login;
pub use device_login::*;

pub use change_repair::RepairTaskClaim;

mod change_queries;
pub use change_queries::*;

/// Operational observations never replace durable audit or authorization.
pub mod telemetry;
