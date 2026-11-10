//! Private, immutable task records. These objects never enter Git's ODB.
mod delivery;
mod model;
pub use delivery::*;
mod run;
pub use run::*;
mod store;
mod validity;
pub use model::*;
pub use store::Store;
pub use validity::{Validity, evaluate};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    Input(alt_input::Error),
    Blob(alt_store::StoreError),
    Invalid(String),
    Precondition(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "task I/O: {e}"),
            Self::Json(e) => write!(f, "task encoding: {e}"),
            Self::Input(e) => std::fmt::Display::fmt(e, f),
            Self::Blob(e) => std::fmt::Display::fmt(e, f),
            Self::Invalid(v) => write!(f, "invalid task record: {v}"),
            Self::Precondition(v) => write!(f, "task precondition failed: {v}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::Input(e) => e.source(),
            Self::Blob(e) => e.source(),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<alt_input::Error> for Error {
    fn from(e: alt_input::Error) -> Self {
        Self::Input(e)
    }
}

impl From<alt_store::StoreError> for Error {
    fn from(e: alt_store::StoreError) -> Self {
        Self::Blob(e)
    }
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
