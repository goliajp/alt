//! PostgreSQL publication authority and a durable immutable POSIX byte store.
mod bytes;
mod committed;
mod database;
mod earlier_requests;
mod gc;
mod jsonb;
pub use jsonb::Jsonb;
mod portable;
mod portable_import;
pub use portable::*;
pub use portable_import::*;
mod open_segments;
mod pack_compaction;
mod pack_native;
mod pack_scrub;
mod pack_sweep;
pub use pack_compaction::PACK_RETIRED_GRACE_MS;
pub use pack_scrub::{NO_REPOSITORY, Reading, Scrubbed, Unread};
pub use pack_sweep::{Writing, writing};
mod pack_view;
pub use pack_view::{ARRAY_TIPS, DEFERRED_REFS, FOUND_ALL, PackView, RefFilter, RefScope};
mod packs;
pub use packs::*;
mod provenance;
mod publication;
mod read;
mod refs;
mod review;
pub use refs::Refs;
mod registration;
mod repository_authority;
mod requests;
mod retention;
pub use publication::*;
pub use read::*;
pub use registration::MAX_REGISTER_CONTENTS;
pub use retention::{
    CREDENTIAL_RETENTION_MS, REQUEST_RETENTION_MS, Removed, RequestsSeen, SESSION_RETENTION_MS,
};
mod schema;
pub use schema::{LZ4_REQUIRED, OLDEST_SCHEMA, UNSUPPORTED_SOURCE_SCHEMA, check_backup_schema};
mod schema_v19;
mod schema_v20;
mod segments;
mod serving;
pub use serving::{SERVED, Serving};
mod store;
mod tasks;
pub use bytes::*;
pub use store::*;

#[derive(Debug)]
pub enum Error {
    Portable(alt_portable::Error),
    Io(std::io::Error),
    Database(alt_pg::Error),
    Protocol(alt_remote_protocol::Error),
    Task(alt_task_service::Error),
    Repository(alt_repository_service::Error),
    Json(alt_json::Error),
    Integrity(&'static str),
    /// an object of a pack `git fsck` refuses, and what it says of it
    Fsck([u8; 20], &'static str),
    Limit(&'static str),
    /// what the request needs is taken by other requests, and was not given back
    /// in the time it had to wait
    Busy(&'static str),
    Precondition(&'static str),
    /// as many sign-ins or device codes were attempted in the minute as may be;
    /// the milliseconds until the minute is over
    Attempts(u64),
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
            Self::Fsck(oid, what) => write!(f, "object {}: {what}", alt_pack_segment::hex(oid)),
            Self::Limit(what) => write!(f, "storage limit exceeded: {what}"),
            Self::Busy(what) => write!(f, "busy with other requests: {what}"),
            Self::Precondition(what) => write!(f, "precondition failed: {what}"),
            Self::Attempts(left) => {
                write!(
                    f,
                    "admission limit; retry after {} seconds",
                    left.div_ceil(1000)
                )
            }
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
            | Self::Fsck(..)
            | Self::Limit(_)
            | Self::Busy(_)
            | Self::Precondition(_)
            | Self::Attempts(_)
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

impl From<alt_pg::Error> for Error {
    fn from(e: alt_pg::Error) -> Self {
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

impl From<alt_json::Error> for Error {
    fn from(e: alt_json::Error) -> Self {
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
pub use review::CurrentReviewAssessment;
pub use workbench::*;

/// Current persisted schema; runtime open validates all migration checksums.
pub const SCHEMA_VERSION: u32 = 23;

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

mod client_credentials;
mod device_login;
mod reading;
pub use client_credentials::*;
pub use device_login::*;

pub use change_repair::RepairTaskClaim;

mod change_queries;
pub use change_queries::*;

/// Operational observations never replace durable audit or authorization.
pub mod telemetry;
