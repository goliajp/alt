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
mod representation;
mod write;

use std::path::Path;
use std::sync::{Arc, Mutex};

use alt_git_codec::{ObjectId, ObjectKind, RawObject};

use cache::DeltaBaseCache;
pub use delta_window::DeltaOptions;
pub use idx::PackIndex;
pub use index_pack::{
    BaseFiles, Bases, EACH, IndexLimits, IndexWork, IndexedPackOnDisk, MAX_DELTA_DEPTH, Memory,
    Taken, Wait, index_pack, index_pack_thin, index_pack_within,
};
/// The walk below the bases of a pack, for whoever verifies packs its own way.
pub mod walk {
    pub use crate::index_pack::{Budget, Deltas, ObjectMemory, Objects, Of, Walk};
}
pub use ingress::*;
pub use pack::{EntryInfo, EntryKind, Pack};
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

    /// Reads the object `oid` if this pack contains it. A pack may hold an
    /// object more than once: it is read from whichever of them leads to
    /// a stored object.
    pub fn read(&self, oid: &ObjectId) -> Result<Option<PackedObject>, PackError> {
        let mut held = Vec::new();
        for i in self.idx.all_of(oid) {
            held.push(self.idx.offset_at(i)?);
        }
        match held.is_empty() {
            true => Ok(None),
            false => self.resolve(held, &mut 0).map(Some),
        }
    }

    /// Reads the object stored at pack offset `offset`, resolving its delta
    /// chain iteratively (deep chains must not recurse).
    pub fn read_at(&self, offset: u64) -> Result<PackedObject, PackError> {
        self.resolve(vec![offset], &mut 0)
    }

    /// [`Self::read_at`], adding to `steps` the entries it looked at.
    #[doc(hidden)]
    pub fn read_at_counting(
        &self,
        offset: u64,
        steps: &mut usize,
    ) -> Result<PackedObject, PackError> {
        self.resolve(vec![offset], steps)
    }

    /// The object one of the entries at `from` makes, the last of them
    /// tried first. `steps` counts the entries looked at.
    ///
    /// A delta names its base by where it is, or by its id, and a pack may
    /// hold that id in several entries, one of which may be a delta of the
    /// very entry that is being made: taken for the base it leads round in
    /// a circle. So the entries are gone through as a graph is searched for
    /// a way to a stored object, each looked at once at the most: the way
    /// found has no entry twice and is no longer than the pack has entries,
    /// and a read ends where there is none.
    fn resolve(&self, from: Vec<u64>, steps: &mut usize) -> Result<PackedObject, PackError> {
        let mut seen = Seen::default();
        // the deltas on the way, and at each depth the entries still to try
        let mut frames: Vec<(u64, EntryInfo)> = Vec::new();
        let mut ahead: Vec<Vec<u64>> = vec![from];
        let mut lacking = false;
        let (kind, mut data) = loop {
            let Some(next) = ahead.last_mut() else {
                return Err(PackError::Format(match lacking {
                    true => "ref-delta base not in pack",
                    false => "delta chain leads to no stored object",
                }));
            };
            let Some(cur) = next.pop() else {
                ahead.pop();
                frames.pop();
                continue;
            };
            if !seen.insert(cur) {
                continue;
            }
            *steps += 1;
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
                    ahead.push(vec![base_at]);
                }
                EntryKind::RefDelta { base } => {
                    // on-disk packs are self-contained; thin packs exist only
                    // on the wire
                    let mut bases = Vec::new();
                    for i in self.idx.all_of(&base) {
                        bases.push(self.idx.offset_at(i)?);
                    }
                    lacking |= bases.is_empty();
                    frames.push((cur, info));
                    ahead.push(bases);
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

/// The entries a read has looked at, by where they are: a few in a row,
/// and a set once there are many.
#[derive(Default)]
struct Seen {
    few: Vec<u64>,
    many: std::collections::HashSet<u64>,
}

impl Seen {
    /// whether `offset` was not seen before
    fn insert(&mut self, offset: u64) -> bool {
        if self.many.is_empty() && self.few.len() < 64 {
            let new = !self.few.contains(&offset);
            if new {
                self.few.push(offset);
            }
            return new;
        }
        if self.many.is_empty() {
            self.many.extend(self.few.drain(..));
        }
        self.many.insert(offset)
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
