//! Git packfile reading: idx lookup, entry decoding, delta resolution.
//!
//! Business-agnostic stone: knows the on-disk pack/idx formats and nothing
//! about alt.

mod idx;
mod pack;

use std::path::Path;

use alt_git_codec::{ObjectId, RawObject};

pub use idx::PackIndex;
pub use pack::{EntryInfo, EntryKind, Pack};

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("io")]
    Io(#[from] std::io::Error),
    #[error("pack format: {0}")]
    Format(&'static str),
    #[error("delta resolution not yet implemented (M1/S5)")]
    UnresolvedDelta,
}

/// A pack plus its index: oid-addressed object reading.
pub struct IndexedPack {
    idx: PackIndex,
    pack: Pack,
}

impl IndexedPack {
    /// Opens `<name>.pack` together with its sibling `<name>.idx`.
    pub fn open(pack_path: &Path, algo: alt_git_codec::HashAlgo) -> Result<Self, PackError> {
        let idx_path = pack_path.with_extension("idx");
        Ok(Self {
            idx: PackIndex::open(&idx_path, algo)?,
            pack: Pack::open(pack_path, algo)?,
        })
    }

    pub fn idx(&self) -> &PackIndex {
        &self.idx
    }

    pub fn pack(&self) -> &Pack {
        &self.pack
    }

    /// Reads the object `oid` if this pack contains it.
    pub fn read(&self, oid: &ObjectId) -> Result<Option<RawObject>, PackError> {
        let Some(i) = self.idx.lookup(oid) else {
            return Ok(None);
        };
        let info = self.pack.entry_info(self.idx.offset_at(i)?)?;
        match info.kind {
            EntryKind::Plain(kind) => Ok(Some(RawObject {
                kind,
                data: self.pack.inflate(info.data_at, info.size)?,
            })),
            EntryKind::OfsDelta { .. } | EntryKind::RefDelta { .. } => {
                Err(PackError::UnresolvedDelta)
            }
        }
    }
}
