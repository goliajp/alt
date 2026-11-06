//! Portable data contains historical identities, never active credentials.
//! An authenticated catalog and intact archive are prerequisites for semantic
//! object/task validation; they do not authorize a destination transaction.
mod archive;
mod catalog;
pub mod http;
mod import;
mod semantic;
pub use archive::*;
pub use catalog::*;
pub use import::*;
pub use semantic::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("portable object graph: {0}")]
    Repository(#[from] alt_repository_service::Error),
    #[error("portable task graph: {0}")]
    Task(#[from] alt_task_service::Error),
    #[error("portable protocol: {0}")]
    Protocol(#[from] alt_remote_protocol::Error),
    #[error("portable provenance: {0}")]
    Provenance(#[from] alt_provenance::Error),
    #[error("portable JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("portable I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid portable package: {0}")]
    Invalid(&'static str),
    #[error("portable limit: {0}")]
    Limit(&'static str),
}
pub type Result<T> = std::result::Result<T, Error>;

mod decimal {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let value = String::deserialize(d)?;
        if value.is_empty()
            || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(serde::de::Error::custom(
                "canonical unsigned decimal required",
            ));
        }
        value.parse().map_err(serde::de::Error::custom)
    }
}
