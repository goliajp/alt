//! Git packfile reading: idx lookup, entry decoding, delta resolution.
//!
//! Pure-logic crate, business-agnostic: knows the on-disk pack/idx formats
//! and nothing about alt.

mod cache;
pub mod delta;
mod delta_encode;
mod delta_window;
mod idx;
mod index_pack;
mod ingress;
mod pack;
mod read;
mod representation;
mod write;

use std::path::Path;
use std::sync::{Arc, Mutex};

use alt_git_codec::{ObjectKind, RawObject};

use cache::DeltaBaseCache;
pub use delta_window::DeltaOptions;
pub use idx::PackIndex;
pub use index_pack::{
    BASE_FILES, BaseFiles, Bases, EACH, IndexLimits, IndexWork, IndexedPackOnDisk, MAX_DELTA_DEPTH,
    Memory, SPILL_FILES, Taken, Wait, index_pack, index_pack_thin, index_pack_within,
};
/// The walk below the bases of a pack, for whoever verifies packs its own way.
pub mod walk {
    pub use crate::index_pack::{Budget, Deltas, ObjectMemory, Objects, Of, Walk};
}
pub use ingress::*;
pub use pack::{EntryInfo, EntryKind, Pack};
pub use read::{HELD_FOR_AN_ENTRY, ReadWork};
pub use representation::{PlainRepresentation, RepresentationLimits};
pub use write::{PackWriter, WrittenPack};

#[derive(Debug)]
pub enum PackError {
    Io(std::io::Error),
    Limit(&'static str),
    Format(&'static str),
    /// what the pack needs is taken by others, and was not given back in
    /// the time the pack had to wait
    Busy(&'static str),
    /// whoever asked for the pack waits for it no longer
    GivenUp,
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(_) => f.write_str("io"),
            Self::Limit(v) => write!(f, "pack resource limit: {v}"),
            Self::Format(v) => write!(f, "pack format: {v}"),
            Self::Busy(v) => write!(f, "busy: {v}"),
            Self::GivenUp => f.write_str("given up"),
        }
    }
}

impl std::error::Error for PackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for PackError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// A fully resolved object read from a pack. Data is shared with the
/// delta-base cache, so cloning is cheap.
#[derive(Debug, Clone)]
pub struct PackedObject {
    pub kind: ObjectKind,
    pub data: Arc<Vec<u8>>,
}

impl PackedObject {
    pub fn to_raw(&self) -> RawObject {
        RawObject {
            kind: self.kind,
            data: (*self.data).clone(),
        }
    }
}

/// A pack plus its index: oid-addressed object reading with full delta
/// resolution.
pub struct IndexedPack {
    idx: PackIndex,
    pack: Pack,
    cache: Mutex<DeltaBaseCache>,
    /// Of the ids the pack holds in several entries, by the place of the
    /// first of them in the index: the entry a read found the object in.
    found_in: Mutex<std::collections::HashMap<u32, u64>>,
}

impl IndexedPack {
    /// Opens `<name>.pack` together with its sibling `<name>.idx`.
    pub fn open(pack_path: &Path, algo: alt_git_codec::HashAlgo) -> Result<Self, PackError> {
        let idx_path = pack_path.with_extension("idx");
        Ok(Self {
            idx: PackIndex::open(&idx_path, algo)?,
            pack: Pack::open(pack_path, algo)?,
            cache: Mutex::new(DeltaBaseCache::new(cache::DEFAULT_BUDGET)),
            found_in: Mutex::default(),
        })
    }

    pub fn idx(&self) -> &PackIndex {
        &self.idx
    }

    pub fn pack(&self) -> &Pack {
        &self.pack
    }
}

/// Inflates the zlib stream at the start of `input` into exactly `output` and returns
/// how many bytes the stream occupied.
pub(crate) fn inflate_entry(input: &[u8], output: &mut [u8]) -> Result<usize, PackError> {
    alt_inflate::inflate_exact(alt_inflate::Format::Zlib, input, output).map_err(
        |error| match error {
            alt_inflate::Error::TooLong | alt_inflate::Error::TooShort => {
                PackError::Format("inflated size does not match entry header")
            }
            other => PackError::from(std::io::Error::from(other)),
        },
    )
}
