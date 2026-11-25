//! Immutable verified Git pack segments.
//!
//! A segment is the pack a client sent, byte for byte, followed by an index
//! built while the pack was verified once: every object inflated, deltas
//! resolved, object ids recomputed, structure parsed and every reference
//! checked to resolve inside the pack or inside the segments already present.
//! After that the file never changes. Reads look objects up by id and trust
//! the per-entry checksum; resends copy the stored compressed bytes.

mod build;
mod carried;
mod chosen;
mod descend;
mod format;
mod generation;
mod ingest;
mod led;
mod limits;
mod mend;
mod merge;
mod object;
mod ordered;
mod passes;
mod plain;
mod reach;
mod readback;
mod room;
mod scan;
mod search;
mod select;
mod send;
mod set;
mod settled;
mod stream;
mod told;
mod trees;
mod unsafe_names;
mod wanted;

pub use alt_git_pack::{BASE_FILES, BaseFiles, IndexWork, Memory, SPILL_FILES, Wait};
pub use build::{Builder, Built, Compress};
pub use format::{CommitInfo, Delta, Record, Segment};
pub use ingest::{Ingested, ingest, ingest_holding, ingest_led, ingest_while_waited_for};
pub use led::Led;
pub use limits::Limits;
pub use mend::{indexed_anew, unread, unread_holding, unread_telling};
pub use merge::{merge, stands_on};
pub use readback::UNREADABLE;
pub use room::least_memory;
pub use room::{DELTA_BYTES, ENTRY_BYTES};
pub use select::Selection;
pub use send::{Piece, build_pack};
pub use set::SegmentSet;
pub use stream::ObjectStream;

pub type Oid = [u8; 20];
/// Tables keyed by ids a peer chooses.
pub(crate) type OidMap<V> = alt_hash::KeyedIdMap<Oid, V>;
pub(crate) type OidSet = alt_hash::KeyedIdSet<Oid>;

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

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Format(&'static str),
    Limit(&'static str),
    Dangling(Oid, Oid),
    MissingBase(Oid),
    Corrupt(&'static str),
    NotFound(Oid),
    /// what the pack needs is taken by others, and was not given back in the time
    /// the pack had to wait
    Busy(&'static str),
    /// whoever asked for the pack waits for it no longer
    GivenUp,
    /// a tree whose names `git fsck` refuses, and what it says of them
    Unsafe(Oid, &'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(_) => f.write_str("io"),
            Self::Format(v) => write!(f, "pack format: {v}"),
            Self::Limit(v) => write!(f, "pack limit: {v}"),
            Self::Dangling(a, b) => write!(
                f,
                "object {} references missing or mistyped object {}",
                hex(a),
                hex(b)
            ),
            Self::MissingBase(v) => write!(f, "delta base {} is not available", hex(v)),
            Self::Corrupt(v) => write!(f, "segment corrupt: {v}"),
            Self::NotFound(v) => write!(f, "object {} not found", hex(v)),
            Self::Busy(v) => write!(f, "pack waits: {v}"),
            Self::GivenUp => f.write_str("pack given up"),
            Self::Unsafe(oid, what) => write!(f, "object {}: {what}", hex(oid)),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<alt_git_pack::PackError> for Error {
    fn from(error: alt_git_pack::PackError) -> Self {
        use alt_git_pack::PackError;
        match error {
            PackError::Io(error) => Self::Io(error),
            PackError::Limit(what) => Self::Limit(what),
            PackError::Format(_) => Self::Format("delta instructions"),
            PackError::Busy(what) => Self::Busy(what),
            PackError::GivenUp => Self::GivenUp,
        }
    }
}

pub fn hex(oid: &Oid) -> String {
    oid.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn object_header(kind: Kind, len: usize) -> String {
    format!("{} {len}\0", kind.name())
}

pub(crate) fn object_id(kind: Kind, data: &[u8]) -> Oid {
    let mut hasher = alt_hash::Sha1::new();
    hasher.update(object_header(kind, data.len()).as_bytes());
    hasher.update(data);
    hasher.finalize()
}
