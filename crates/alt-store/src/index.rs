//! Chunk id → pack location: index runs over every pack's records, in pack
//! then offset order, plus the records appended since the last run. A cache
//! of what the packs say: reads check the record they land on against the
//! id they asked for.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use alt_hash::IdMap;

use crate::runs::{Guard, Key, Runs};
use crate::{ChunkId, StoreError};

/// Tail length, in records, from which a flush folds the tail into the runs.
pub(crate) const INDEX_AT: usize = 256;
/// Kept locations before the table is emptied and filled again.
const KEPT_MAX: usize = 1 << 20;
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

    /// Where the records after this location begin in pack `seq`: here in
    /// its own pack, at the first record of any other.
    pub fn offset_in(self, seq: u32) -> u64 {
        if seq == self.seq {
            self.offset
        } else {
            crate::pack::HEADER_LEN as u64
        }
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
    /// locations found through the runs, kept once [`Self::keep_reads`]
    /// was called: a handle that lives long finds the same chunks again
    /// and again, and a search of the runs costs several cache misses
    kept: Option<Mutex<IdMap<ChunkId, Location>>>,
}

impl ChunkIndex {
    /// Loads the runs that start at `base`; the caller then feeds the
    /// records past [`Self::covered`] through [`Self::insert`].
    pub fn open(
        store_dir: &Path,
        base: Location,
        named: Option<&[(u64, u64)]>,
        guard_at: impl Fn(Location) -> Option<Guard>,
    ) -> Result<Self, StoreError> {
        let dir = store_dir.join("chunks.idx");
        let base = base.position();
        let guard = |end| guard_at(Location::at(end));
        let runs = match named {
            Some(named) => Runs::open_named(&dir, base, named, guard)?,
            None => Runs::open(&dir, base, guard)?,
        };
        let end = runs.covered();
        Ok(Self {
            dir,
            base,
            runs,
            tail: Vec::new(),
            latest: IdMap::default(),
            end,
            kept: None,
        })
    }

    /// Where the records not yet in the runs begin.
    pub fn covered(&self) -> Location {
        Location::at(self.runs.covered())
    }

    /// Notes a record at `loc` whose bytes stop at `end` in the same pack.
    pub fn insert(&mut self, id: ChunkId, loc: Location, end: u64) {
        if let Some(kept) = &mut self.kept {
            // written again: the record just appended is the one to find
            kept.get_mut().unwrap().remove(&id);
        }
        self.tail.push((id, loc));
        self.latest.insert(id, loc);
        self.end = Location {
            seq: loc.seq,
            offset: end,
        }
        .position();
    }

    pub fn get(&self, id: ChunkId) -> Result<Option<Location>, StoreError> {
        Ok(self.find(id)?.map(|(loc, _)| loc))
    }

    /// Where `id` is, and whether that comes from the runs rather than
    /// from records this handle read itself. The runs are trusted on the
    /// strength of their guard alone, so what follows from a hit in them
    /// (above all skipping a write) wants the record looked at first.
    pub fn find(&self, id: ChunkId) -> Result<Option<(Location, bool)>, StoreError> {
        if let Some(loc) = self.latest.get(&id) {
            return Ok(Some((*loc, false)));
        }
        if let Some(kept) = &self.kept
            && let Some(loc) = kept.lock().unwrap().get(&id)
        {
            return Ok(Some((*loc, true)));
        }
        let found = self.runs.find_last(&id.0)?.map(Location::at);
        if let (Some(kept), Some(loc)) = (&self.kept, found) {
            let mut kept = kept.lock().unwrap();
            if kept.len() >= KEPT_MAX {
                kept.clear();
            }
            kept.insert(id, loc);
        }
        Ok(found.map(|loc| (loc, true)))
    }

    /// Keeps the locations found through the runs from now on.
    pub fn keep_reads(&mut self) {
        self.kept.get_or_insert_default();
    }

    /// Drops the kept locations: the runs changed under them.
    fn forget(&mut self) {
        if let Some(kept) = &mut self.kept {
            kept.get_mut().unwrap().clear();
        }
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

    /// The range of each run, oldest first (see [`Runs::open_named`]).
    pub fn chain(&self) -> Vec<(u64, u64)> {
        self.runs.chain()
    }

    /// Re-hashes the run files against their checksums.
    pub fn verify_runs(&self) -> Result<(), StoreError> {
        self.runs.verify()
    }

    /// Entries the runs hold.
    pub fn run_entries(&self) -> usize {
        self.runs.count()
    }

    /// Checks that the index knows the record a pack walk found at `loc`,
    /// and resolves its id to that record or a later one. Says whether the
    /// record is one the runs cover.
    pub fn check_record(&self, id: ChunkId, loc: Location) -> Result<bool, StoreError> {
        let position = loc.position();
        let missing = StoreError::Format("chunk index misses a record");
        let latest = self.get(id)?.ok_or(missing)?;
        if latest.position() < position {
            return Err(StoreError::Format("chunk index names a superseded record"));
        }
        if position >= self.runs.covered() {
            return Ok(false);
        }
        let mut found = false;
        self.runs.find_each(&id.0, |at| found |= at == position)?;
        if found {
            Ok(true)
        } else {
            Err(StoreError::Format("chunk index misses a record"))
        }
    }

    /// The position a run written now would end at, when the tail is long
    /// enough to be worth one.
    pub fn pending(&self) -> Option<Location> {
        (self.tail.len() >= INDEX_AT).then(|| Location::at(self.end))
    }

    /// Moves the tail into the runs. Run under the write lock with every
    /// record on disk already in the tail; `guard` is the pack bytes before
    /// [`Self::pending`], `guard_at` reads the same for any position.
    ///
    /// Returns false, having changed nothing, when the runs on disk stop
    /// short of where this handle's tail begins: a run written now would
    /// claim records it has no entries for. The caller reloads and retries.
    pub fn index_tail(
        &mut self,
        guard: Guard,
        guard_at: impl Fn(Location) -> Option<Guard>,
        durable: bool,
    ) -> Result<bool, StoreError> {
        // another process may have indexed part of our tail already
        let runs = Runs::open(&self.dir, self.base, |end| guard_at(Location::at(end)))?;
        let covered = runs.covered();
        if covered < self.runs.covered() {
            return Ok(false);
        }
        // the runs and the tail change together, so a failed write below
        // leaves the index whole
        self.forget();
        self.runs = runs;
        self.tail.retain(|(_, loc)| loc.position() >= covered);
        self.latest = self.tail.iter().copied().collect();
        if self.tail.len() >= INDEX_AT {
            let entries: Vec<(Key, u64)> = self
                .tail
                .iter()
                .map(|(id, loc)| (id.0, loc.position()))
                .collect();
            self.runs
                .extend(&entries, covered, self.end, guard, durable)?;
            self.tail.clear();
            self.latest.clear();
        }
        Ok(true)
    }

    /// Where the runs [`Self::truncate`] would keep for `target` stop.
    pub fn covered_up_to(&self, target: Location) -> Location {
        Location::at(self.runs.covered_up_to(target.position()))
    }

    /// Drops the runs reaching past `target`, before a pack is cut back.
    pub fn truncate(&mut self, target: Location) -> Result<(), StoreError> {
        self.forget();
        self.runs.truncate(target.position())
    }

    /// Drops every run, before the packs they point into are replaced.
    pub fn clear(&mut self) -> Result<(), StoreError> {
        self.forget();
        self.runs.truncate(0)
    }

    pub fn remove_stale(&mut self) -> Result<(), StoreError> {
        self.runs.remove_stale()
    }
}
