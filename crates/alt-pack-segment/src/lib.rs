//! Immutable verified Git pack segments.
//!
//! A segment is the pack a client sent, byte for byte, followed by an index
//! built while the pack was verified once: every object inflated, deltas
//! resolved, object ids recomputed, structure parsed and every reference
//! checked to resolve inside the pack or inside the segments already present.
//! After that the file never changes. Reads look objects up by id and trust
//! the per-entry checksum; resends copy the stored compressed bytes.

mod format;
mod ingest;
mod object;
mod scan;
mod send;
mod set;

pub use format::{CommitInfo, Record, Segment};
pub use ingest::{Ingested, Limits, ingest};
pub use send::{Selection, build_pack};
pub use set::SegmentSet;

pub type Oid = [u8; 20];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Commit = 1,
    Tree = 2,
    Blob = 3,
    Tag = 4,
}

impl Kind {
    pub(crate) fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Commit),
            2 => Some(Self::Tree),
            3 => Some(Self::Blob),
            4 => Some(Self::Tag),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Tree => "tree",
            Self::Blob => "blob",
            Self::Tag => "tag",
        }
    }
}

/// set when the pack carried the same object more than once
pub const FLAG_DUPLICATES: u32 = 1;
/// set when the pack carried an object an earlier segment already holds
pub const FLAG_OVERLAP: u32 = 2;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io")]
    Io(#[from] std::io::Error),
    #[error("pack format: {0}")]
    Format(&'static str),
    #[error("pack limit: {0}")]
    Limit(&'static str),
    #[error("object {} references missing or mistyped object {}", hex(.0), hex(.1))]
    Dangling(Oid, Oid),
    #[error("delta base {} is not available", hex(.0))]
    MissingBase(Oid),
    #[error("segment corrupt: {0}")]
    Corrupt(&'static str),
    #[error("object {} not found", hex(.0))]
    NotFound(Oid),
}

pub fn hex(oid: &Oid) -> String {
    oid.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn object_id(kind: Kind, data: &[u8]) -> Oid {
    use sha1::Digest;
    let mut hasher = sha1::Sha1::new();
    hasher.update(kind.name().as_bytes());
    hasher.update(b" ");
    hasher.update(data.len().to_string().as_bytes());
    hasher.update([0]);
    hasher.update(data);
    hasher.finalize().into()
}
