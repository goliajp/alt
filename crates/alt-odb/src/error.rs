//! The object database's error type.

use alt_git_codec::ObjectId;
use alt_store::{BlobId, StoreError};

#[derive(Debug)]
pub enum OdbError {
    Io(std::io::Error),
    Store(StoreError),
    Format(&'static str),
    HashMismatch { claimed: ObjectId, actual: ObjectId },
    SizeMismatch(ObjectId, u64),
    Tier1Record(BlobId, crate::tier1::RecordError),
    Tier1Recompose(BlobId),
    Tier1HashMismatch(BlobId),
}

impl std::fmt::Display for OdbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(_) => f.write_str("io"),
            Self::Store(_) => f.write_str("store"),
            Self::Format(what) => write!(f, "odb format: {what}"),
            Self::HashMismatch { claimed, actual } => {
                write!(f, "object {claimed} does not hash to its id (got {actual})")
            }
            Self::SizeMismatch(oid, len) => write!(
                f,
                "object {oid} maps to {len} bytes but the store returned a different length"
            ),
            Self::Tier1Record(id, e) => write!(f, "tier1 record for {id} is malformed: {e}"),
            Self::Tier1Recompose(id) => write!(f, "tier1 recompose failed for {id}"),
            Self::Tier1HashMismatch(id) => {
                write!(f, "tier1 recomposed bytes do not hash to {id}")
            }
        }
    }
}

impl std::error::Error for OdbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Store(e) => Some(e),
            Self::Format(_)
            | Self::HashMismatch { .. }
            | Self::SizeMismatch(..)
            | Self::Tier1Record(..)
            | Self::Tier1Recompose(_)
            | Self::Tier1HashMismatch(_) => None,
        }
    }
}

impl From<std::io::Error> for OdbError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<StoreError> for OdbError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}
