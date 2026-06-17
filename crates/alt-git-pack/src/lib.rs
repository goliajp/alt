//! Git packfile reading: idx lookup, entry decoding, delta resolution.
//!
//! Pure-logic crate, business-agnostic: knows the on-disk pack/idx formats
//! and nothing about alt.

mod cache;
pub mod delta;
mod idx;
mod index_pack;
mod pack;
mod write;

use std::path::Path;
use std::sync::{Arc, Mutex};

use alt_git_codec::{ObjectId, ObjectKind, RawObject};

use cache::DeltaBaseCache;
pub use idx::PackIndex;
pub use index_pack::{IndexedPackOnDisk, index_pack};
pub use pack::{EntryInfo, EntryKind, Pack};
pub use write::{PackWriter, WrittenPack};

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("io")]
    Io(#[from] std::io::Error),
    #[error("pack format: {0}")]
    Format(&'static str),
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
}

impl IndexedPack {
    /// Opens `<name>.pack` together with its sibling `<name>.idx`.
    pub fn open(pack_path: &Path, algo: alt_git_codec::HashAlgo) -> Result<Self, PackError> {
        let idx_path = pack_path.with_extension("idx");
        Ok(Self {
            idx: PackIndex::open(&idx_path, algo)?,
            pack: Pack::open(pack_path, algo)?,
            cache: Mutex::new(DeltaBaseCache::new(cache::DEFAULT_BUDGET)),
        })
    }

    pub fn idx(&self) -> &PackIndex {
        &self.idx
    }

    pub fn pack(&self) -> &Pack {
        &self.pack
    }

    /// Reads the object `oid` if this pack contains it.
    pub fn read(&self, oid: &ObjectId) -> Result<Option<PackedObject>, PackError> {
        match self.idx.lookup(oid) {
            None => Ok(None),
            Some(i) => self.read_at(self.idx.offset_at(i)?).map(Some),
        }
    }

    /// Reads the object stored at pack offset `offset`, resolving its delta
    /// chain iteratively (deep chains must not recurse).
    pub fn read_at(&self, offset: u64) -> Result<PackedObject, PackError> {
        // walk down the chain until a cached result or a plain entry
        let mut frames: Vec<(u64, EntryInfo)> = Vec::new();
        let mut cur = offset;
        let (kind, mut data) = loop {
            if let Some(hit) = self.cache.lock().unwrap().get(cur) {
                break hit;
            }
            let info = self.pack.entry_info(cur)?;
            match info.kind {
                EntryKind::Plain(kind) => {
                    let data = Arc::new(self.pack.inflate(info.data_at, info.size)?);
                    self.cache.lock().unwrap().put(cur, kind, data.clone());
                    break (kind, data);
                }
                EntryKind::OfsDelta { base_at } => {
                    frames.push((cur, info));
                    cur = base_at;
                }
                EntryKind::RefDelta { base } => {
                    // on-disk packs are self-contained; thin packs exist only
                    // on the wire
                    let i = self
                        .idx
                        .lookup(&base)
                        .ok_or(PackError::Format("ref-delta base not in pack"))?;
                    frames.push((cur, info));
                    cur = self.idx.offset_at(i)?;
                }
            }
        };
        // apply deltas back up, caching every intermediate result
        for (off, info) in frames.iter().rev() {
            let raw_delta = self.pack.inflate(info.data_at, info.size)?;
            let out = Arc::new(delta::apply(&data, &raw_delta)?);
            self.cache.lock().unwrap().put(*off, kind, out.clone());
            data = out;
        }
        Ok(PackedObject { kind, data })
    }
}
