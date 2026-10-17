//! Private, immutable task records. These objects never enter Git's ODB.
mod model;
mod run;
pub use run::*;
mod store;
mod validity;
pub use model::*;
pub use store::Store;
pub use validity::{Validity, evaluate};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("task I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("task encoding: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Input(#[from] alt_input::Error),
    #[error(transparent)]
    Blob(#[from] alt_store::StoreError),
    #[error("invalid task record: {0}")]
    Invalid(String),
    #[error("task precondition failed: {0}")]
    Precondition(String),
}
pub type Result<T> = std::result::Result<T, Error>;

pub fn check_id(id: &str) -> Result<()> {
    if id.len() != 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(Error::Invalid("expected lowercase BLAKE3 id".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
