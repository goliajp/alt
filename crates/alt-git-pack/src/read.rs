//! Reading an object out of an indexed pack.
//!
//! A delta names its base by where it is, or by its id, and a pack may hold
//! that id in many entries, any of which may be a delta of the very entry
//! that is being made. So the entries are searched as a graph is, breadth
//! first, for the stored entry or the cached object fewest deltas away.
//!
//! What a read of a pack of `n` entries takes, whatever the pack is made
//! of ([`ReadWork`] counts each):
//!
//! - it looks at an entry once at the most: `n` entries at the most;
//! - it goes through the entries of an id once at the most, and the ids
//!   share no entry: `n` steps at the most, behind two searches of the
//!   index for each delta by id it looked at;
//! - the entry it found an id of several entries in is kept with the pack,
//!   and the reads after it come to that entry alone: one who reads every
//!   entry of a pack goes through the entries of an id once, not once for
//!   each delta made of it. A read that fails by the entry kept is made
//!   again without what is kept, so that it finds what it found before any
//!   was;
//! - it applies the deltas of the way it found, the shortest there is:
//!   [`MAX_DELTA_DEPTH`] at the most, and an object further down is not read;
//! - besides the object, one delta and what the cache keeps, it holds
//!   sixteen bytes for an entry it looked at, a set of where those are and
//!   a set of the ids gone through: under [`HELD_FOR_AN_ENTRY`] bytes for
//!   each, while the lists grow as well.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use alt_git_codec::ObjectId;

use crate::{EntryKind, IndexedPack, MAX_DELTA_DEPTH, PackError, PackedObject, delta};

/// The bytes a read holds at the most for each entry it looked at.
pub const HELD_FOR_AN_ENTRY: usize = 128;

/// What a read did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReadWork {
    /// the entries it looked at
    pub looked: usize,
    /// the entries of ids it went through for the deltas made of them
    pub ranged: usize,
    /// the deltas it applied
    pub applied: usize,
}

/// no entry: the one a read began at is made of none
const NONE: u32 = u32::MAX;

/// The entries found so far, in the order they are looked at, each with the
/// entry that is a delta of it.
#[derive(Default)]
struct Found {
    entries: Vec<(u64, u32)>,
    seen: Seen,
    /// the ids of several entries that were gone through, by their first
    gone_through: HashSet<u32>,
    /// an id was taken for the one entry an earlier read found it in
    kept: bool,
    /// no id is taken so: every entry of each is gone through
    anew: bool,
}

/// Where the entries looked at are, and at which place of those found: a few
/// in a row, and a map once there are many. Most reads are of a short chain
/// and have no need of a map.
#[derive(Default)]
struct Seen {
    few: Vec<u64>,
    many: HashMap<u64, u32>,
}

impl Seen {
    /// the place of `offset`, where it was seen
    fn place(&self, offset: u64) -> Option<u32> {
        match self.many.is_empty() {
            true => (self.few.iter().position(|seen| *seen == offset)).map(|at| at as u32),
            false => self.many.get(&offset).copied(),
        }
    }

    /// whether `offset` was not seen before; it is seen at `place` from now on
    fn insert(&mut self, offset: u64, place: u32) -> bool {
        if self.many.is_empty() && self.few.len() < 64 {
            let new = !self.few.contains(&offset);
            if new {
                self.few.push(offset);
            }
            return new;
        }
        if self.many.is_empty() {
            let few = self.few.drain(..).enumerate();
            self.many.extend(few.map(|(at, seen)| (seen, at as u32)));
        }
        let new = !self.many.contains_key(&offset);
        if new {
            self.many.insert(offset, place);
        }
        new
    }
}

impl Found {
    /// whether the entry was not found before
    fn entry(&mut self, offset: u64, under: u32) -> bool {
        let new = self.seen.insert(offset, self.entries.len() as u32);
        if new {
            self.entries.push((offset, under));
        }
        new
    }

    /// whether the entry at `place` is one the entry `under` is made from, or
    /// that entry itself: a way down that comes back to it goes round
    fn below(&self, place: u32, mut under: u32) -> bool {
        while under != NONE {
            if under == place {
                return true;
            }
            under = self.entries[under as usize].1;
        }
        false
    }
}

impl IndexedPack {
    /// Reads the object `oid` if this pack contains it. A pack may hold an
    /// object more than once: it is read from the entry fewest deltas away
    /// from a stored one.
    pub fn read(&self, oid: &ObjectId) -> Result<Option<PackedObject>, PackError> {
        self.read_counting(oid, &mut ReadWork::default())
    }

    /// Reads the object stored at pack offset `offset`.
    pub fn read_at(&self, offset: u64) -> Result<PackedObject, PackError> {
        self.read_at_counting(offset, &mut ReadWork::default())
    }

    /// [`Self::read`], adding to `work` what it took, for a test to count
    #[doc(hidden)]
    pub fn read_counting(
        &self,
        oid: &ObjectId,
        work: &mut ReadWork,
    ) -> Result<Option<PackedObject>, PackError> {
        let all = self.idx.all_of(oid);
        if all.is_empty() {
            return Ok(None);
        }
        let begin =
            |found: &mut Found, work: &mut ReadWork| self.all(found, all.clone(), NONE, work);
        self.read_from(&begin, Some(all.clone()), work).map(Some)
    }

    /// [`Self::read_at`], adding to `work` what it took, for a test to count
    #[doc(hidden)]
    pub fn read_at_counting(
        &self,
        offset: u64,
        work: &mut ReadWork,
    ) -> Result<PackedObject, PackError> {
        let begin = |found: &mut Found, _: &mut ReadWork| {
            found.entry(offset, NONE);
            Ok(())
        };
        self.read_from(&begin, None, work)
    }

    /// The object the entries `begin` finds make; `top` are those of its id,
    /// where it is read by that.
    fn read_from(
        &self,
        begin: &dyn Fn(&mut Found, &mut ReadWork) -> Result<(), PackError>,
        top: Option<std::ops::Range<u32>>,
        work: &mut ReadWork,
    ) -> Result<PackedObject, PackError> {
        let mut found = Found::default();
        begin(&mut found, work)?;
        match self.resolve(&mut found, top.clone(), work) {
            // the entry kept may be further down than another of its id is
            Err(_) if found.kept => {
                let mut found = Found {
                    anew: true,
                    ..Found::default()
                };
                begin(&mut found, work)?;
                self.resolve(&mut found, top, work)
            }
            read => read,
        }
    }

    /// Keeps that the object of the entries `all` was found in the one at
    /// `offset`, where there are several.
    fn keep(&self, all: std::ops::Range<u32>, offset: u64) {
        if all.len() > 1 {
            self.found_in.lock().unwrap().insert(all.start, offset);
        }
    }

    /// Finds the entries `all` of one id, each a base of the entry `under`.
    fn all(
        &self,
        found: &mut Found,
        all: std::ops::Range<u32>,
        under: u32,
        work: &mut ReadWork,
    ) -> Result<(), PackError> {
        if all.len() > 1 && !found.anew {
            let kept = self.found_in.lock().unwrap().get(&all.start).copied();
            // the entry kept may be found already: beside the way this read
            // came, where it is reached from there; on that way, where the
            // object is then looked for in the others
            if let Some(offset) = kept {
                let place = found.seen.place(offset);
                if place.is_none_or(|place| !found.below(place, under)) {
                    found.entry(offset, under);
                    found.kept = true;
                    work.ranged += 1;
                    return Ok(());
                }
            }
        }
        // once is enough: whatever comes to this id again finds them found
        if all.len() > 1 && !found.gone_through.insert(all.start) {
            return Ok(());
        }
        for i in all {
            work.ranged += 1;
            found.entry(self.idx.offset_at(i)?, under);
        }
        Ok(())
    }

    /// The object one of the entries `found` begins with makes.
    fn resolve(
        &self,
        found: &mut Found,
        top: Option<std::ops::Range<u32>>,
        work: &mut ReadWork,
    ) -> Result<PackedObject, PackError> {
        let mut lacking = false;
        // where the entries one delta further down begin, and how far down these are
        let (mut further, mut depth) = (found.entries.len(), 0);
        let mut at = 0;
        let (kind, mut data) = loop {
            if at == found.entries.len() {
                return Err(PackError::Format(match lacking {
                    true => "ref-delta base not in pack",
                    false => "delta chain leads to no stored object",
                }));
            }
            if at == further {
                (further, depth) = (found.entries.len(), depth + 1);
                if depth > MAX_DELTA_DEPTH {
                    return Err(PackError::Format("delta chain too deep"));
                }
            }
            let cur = found.entries[at].0;
            work.looked += 1;
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
                    found.entry(base_at, at as u32);
                }
                EntryKind::RefDelta { base } => {
                    // on-disk packs are self-contained; thin packs exist only
                    // on the wire
                    let all = self.idx.all_of(&base);
                    lacking |= all.is_empty();
                    self.all(found, all, at as u32, work)?;
                }
            }
            at += 1;
        };
        // apply the deltas back up, caching every intermediate result
        let (mut below, mut under) = found.entries[at];
        while under != NONE {
            let (offset, above) = found.entries[under as usize];
            let info = self.pack.entry_info(offset)?;
            if let EntryKind::RefDelta { base } = &info.kind {
                self.keep(self.idx.all_of(base), below);
            }
            let raw_delta = self.pack.inflate(info.data_at, info.size)?;
            let out = Arc::new(delta::apply(&data, &raw_delta)?);
            self.cache.lock().unwrap().put(offset, kind, out.clone());
            work.applied += 1;
            (data, below, under) = (out, offset, above);
        }
        if let Some(top) = top {
            self.keep(top, below);
        }
        Ok(PackedObject { kind, data })
    }
}
