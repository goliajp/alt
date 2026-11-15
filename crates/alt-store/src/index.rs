//! Chunk id → pack location: index runs over every pack's records, in pack
//! then offset order, plus the records appended since the last run. A cache
//! of what the packs say: reads check the record they land on against the
//! id they asked for.

use std::path::{Path, PathBuf};

use alt_hash::IdMap;

use crate::runs::{Guard, Key, Runs};
use crate::{ChunkId, StoreError};

/// Tail length, in records, from which a flush folds the tail into the runs.
pub(crate) const INDEX_AT: usize = 256;
const OFFSET_BITS: u32 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Location {
    pub seq: u32,
    pub offset: u64,
}

impl Location {
    /// The location as one number that grows in append order.
    pub fn position(self) -> u64 {
        debug_assert!(self.offset < 1 << OFFSET_BITS);
        (self.seq as u64) << OFFSET_BITS | self.offset
    }

    pub fn at(position: u64) -> Self {
        Self {
            seq: (position >> OFFSET_BITS) as u32,
            offset: position & ((1 << OFFSET_BITS) - 1),
        }
    }
}

pub(crate) struct ChunkIndex {
    dir: PathBuf,
    base: u64,
    runs: Runs,
    /// records past the runs, in append order
    tail: Vec<(ChunkId, Location)>,
    latest: IdMap<ChunkId, Location>,
    /// position just past the last record known
    end: u64,
}

impl ChunkIndex {
    /// Loads the runs that start at `base`; the caller then feeds the
    /// records past [`Self::covered`] through [`Self::insert`].
    pub fn open(
        store_dir: &Path,
        base: Location,
        guard_at: impl Fn(Location) -> Option<Guard>,
    ) -> Result<Self, StoreError> {
        let dir = store_dir.join("chunks.idx");
        let base = base.position();
        let runs = Runs::open(&dir, base, |end| guard_at(Location::at(end)))?;
        let end = runs.covered();
        Ok(Self {
            dir,
            base,
            runs,
            tail: Vec::new(),
            latest: IdMap::default(),
            end,
        })
    }

    /// Where the records not yet in the runs begin.
    pub fn covered(&self) -> Location {
        Location::at(self.runs.covered())
    }

    /// Notes a record at `loc` whose bytes stop at `end` in the same pack.
    pub fn insert(&mut self, id: ChunkId, loc: Location, end: u64) {
        self.tail.push((id, loc));
        self.latest.insert(id, loc);
        self.end = Location {
            seq: loc.seq,
            offset: end,
        }
        .position();
    }

    pub fn get(&self, id: ChunkId) -> Result<Option<Location>, StoreError> {
        if let Some(loc) = self.latest.get(&id) {
            return Ok(Some(*loc));
        }
        Ok(self.runs.find_last(&id.0)?.map(Location::at))
    }

    pub fn is_empty(&self) -> bool {
        self.tail.is_empty() && self.runs.count() == 0
    }

    /// Every chunk's current location.
    pub fn all(&self) -> IdMap<ChunkId, Location> {
        let mut all = IdMap::default();
        all.reserve(self.runs.count() + self.tail.len());
        self.runs.for_each(|key, position| {
            let id = ChunkId(key.try_into().unwrap());
            let loc = Location::at(position);
            // runs sort by key then position, oldest run first
            all.insert(id, loc);
        });
        for (id, loc) in &self.tail {
            all.insert(*id, *loc);
        }
        all
    }

    /// Every record of pack `seq`, superseded ones included.
    pub fn pack_entries(&self, seq: u32) -> Vec<(ChunkId, u64)> {
        let mut entries = Vec::new();
        self.runs.for_each(|key, position| {
            let loc = Location::at(position);
            if loc.seq == seq {
                entries.push((ChunkId(key.try_into().unwrap()), loc.offset));
            }
        });
        entries.extend(
            self.tail
                .iter()
                .filter(|(_, loc)| loc.seq == seq)
                .map(|(id, loc)| (*id, loc.offset)),
        );
        entries
    }

    /// The position a run written now would end at, when the tail is long
    /// enough to be worth one.
    pub fn pending(&self) -> Option<Location> {
        (self.tail.len() >= INDEX_AT).then(|| Location::at(self.end))
    }

    /// Moves the tail into the runs. Run under the write lock with every
    /// record on disk already in the tail; `guard` is the 16 pack bytes
    /// before [`Self::pending`], `guard_at` reads the same for any position.
    pub fn index_tail(
        &mut self,
        guard: Guard,
        guard_at: impl Fn(Location) -> Option<Guard>,
        durable: bool,
    ) -> Result<(), StoreError> {
        // another process may have indexed part of our tail already
        let mut runs = Runs::open(&self.dir, self.base, |end| guard_at(Location::at(end)))?;
        let covered = runs.covered();
        self.tail.retain(|(_, loc)| loc.position() >= covered);
        if self.tail.len() >= INDEX_AT {
            let entries: Vec<(Key, u64)> = self
                .tail
                .iter()
                .map(|(id, loc)| (id.0, loc.position()))
                .collect();
            runs.extend(&entries, self.end, guard, durable)?;
            self.tail.clear();
        }
        self.runs = runs;
        self.latest = self.tail.iter().copied().collect();
        Ok(())
    }

    /// Drops the runs reaching past `target`, before a pack is cut back.
    pub fn truncate(&mut self, target: Location) -> Result<(), StoreError> {
        self.runs.truncate(target.position())
    }

    /// Drops every run, before the packs they point into are replaced.
    pub fn clear(&mut self) -> Result<(), StoreError> {
        self.runs.truncate(0)
    }

    pub fn remove_stale(&mut self) -> Result<(), StoreError> {
        self.runs.remove_stale()
    }
}
