//! PostgreSQL publication authority and a durable immutable POSIX byte store.
mod bytes;
mod database;
mod gc;
mod portable;
mod portable_import;
pub use portable::*;
pub use portable_import::*;
mod packs;
pub use packs::*;
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

#[derive(Debug)]
pub enum Error {
    Portable(alt_portable::Error),
    Io(std::io::Error),
    Database(postgres::Error),
    Protocol(alt_remote_protocol::Error),
    Task(alt_task_service::Error),
    Repository(alt_repository_service::Error),
    Json(serde_json::Error),
    Integrity(&'static str),
    Limit(&'static str),
    Precondition(&'static str),
    AuthorityUnavailable,
    Unauthorized,
    OutcomeUnknown,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Portable(e) => write!(f, "portable migration: {e}"),
            Self::Io(e) => write!(f, "storage I/O: {e}"),
            Self::Database(e) => write!(f, "database: {e}"),
            Self::Protocol(e) => write!(f, "protocol: {e}"),
            Self::Task(e) => write!(f, "task: {e}"),
            Self::Repository(e) => write!(f, "repository: {e}"),
            Self::Json(e) => write!(f, "encoding: {e}"),
            Self::Integrity(what) => write!(f, "integrity failure: {what}"),
            Self::Limit(what) => write!(f, "storage limit exceeded: {what}"),
            Self::Precondition(what) => write!(f, "precondition failed: {what}"),
            Self::AuthorityUnavailable => {
                f.write_str("current account authority is temporarily unavailable")
            }
            Self::Unauthorized => f.write_str("access denied"),
            Self::OutcomeUnknown => {
                f.write_str("transaction outcome unknown; query the original request")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Portable(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::Database(e) => Some(e),
            Self::Protocol(e) => Some(e),
            Self::Task(e) => Some(e),
            Self::Repository(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::Integrity(_)
            | Self::Limit(_)
            | Self::Precondition(_)
            | Self::AuthorityUnavailable
            | Self::Unauthorized
            | Self::OutcomeUnknown => None,
        }
    }
}

impl From<alt_portable::Error> for Error {
    fn from(e: alt_portable::Error) -> Self {
        Self::Portable(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<postgres::Error> for Error {
    fn from(e: postgres::Error) -> Self {
        Self::Database(e)
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

impl From<alt_repository_service::Error> for Error {
    fn from(e: alt_repository_service::Error) -> Self {
        Self::Repository(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
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
pub const SCHEMA_VERSION: u32 = 18;

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
