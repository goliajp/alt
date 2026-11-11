//! The object ids one pack carries, laid out for the lookups its verification makes.
//!
//! Every tree entry and every commit parent is looked up, which is far more lookups
//! than there are objects, and ids are hashes, so one lookup tells nothing about
//! where the next will land. The ids therefore lie apart from everything else known
//! about the objects, twenty bytes each and in order, behind a table of where each
//! value of their leading bits starts: a lookup reads one table entry and the one or
//! two ids that share those bits.

use crate::{Error, Kind, Oid, SegmentSet, object::Parsed};

pub(crate) struct Carried {
    oids: Vec<Oid>,
    kinds: Vec<Kind>,
    /// ids whose leading bits are `p` are at `starts[p]..starts[p + 1]`
    starts: Vec<u32>,
    shift: u32,
}

impl Carried {
    /// `objects` in ascending order of id, each id once
    pub fn new(objects: impl ExactSizeIterator<Item = (Oid, Kind)>) -> Self {
        // as many table entries as objects, to the power of two below
        let bits = objects.len().max(1).ilog2().min(24);
        let shift = 32 - bits;
        let mut starts = vec![0u32; (1usize << bits) + 1];
        let (mut oids, mut kinds) = (
            Vec::with_capacity(objects.len()),
            Vec::with_capacity(objects.len()),
        );
        for (oid, kind) in objects {
            starts[Self::leading(&oid, shift) + 1] += 1;
            oids.push(oid);
            kinds.push(kind);
        }
        let mut total = 0;
        for start in &mut starts {
            total += *start;
            *start = total;
        }
        Self {
            oids,
            kinds,
            starts,
            shift,
        }
    }

    fn leading(oid: &Oid, shift: u32) -> usize {
        // 64 bits wide because a table of one entry shifts all 32 away
        (u64::from(u32::from_be_bytes([oid[0], oid[1], oid[2], oid[3]])) >> shift) as usize
    }

    pub fn kind(&self, oid: &Oid) -> Option<Kind> {
        let leading = Self::leading(oid, self.shift);
        let (low, high) = (
            self.starts[leading] as usize,
            self.starts[leading + 1] as usize,
        );
        self.oids[low..high]
            .binary_search(oid)
            .ok()
            .map(|at| self.kinds[low + at])
    }

    /// Fails unless every object that `parsed`, the content of `from`, refers to is
    /// carried or held by `existing`, as the kind it is referred to as.
    pub fn check(&self, existing: &SegmentSet, from: &Oid, parsed: &Parsed) -> Result<(), Error> {
        let one = |to: &Oid, kind: Kind| -> Result<(), Error> {
            let found = match self.kind(to) {
                Some(found) => Some(found),
                None => existing.kind(to)?,
            };
            if found == Some(kind) {
                Ok(())
            } else {
                Err(Error::Dangling(*from, *to))
            }
        };
        match parsed {
            Parsed::Blob => Ok(()),
            Parsed::Commit { tree, parents } => {
                one(tree, Kind::Tree)?;
                parents
                    .iter()
                    .try_for_each(|parent| one(parent, Kind::Commit))
            }
            Parsed::Tree(entries) => entries.iter().try_for_each(|(oid, kind)| one(oid, *kind)),
            Parsed::Tag { object, kind } => one(object, *kind),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn kind_of(oid: &Oid) -> Kind {
        Kind::from_code(oid[19] % 4 + 1).unwrap()
    }

    /// `count` distinct ids in order; with `crowded`, most share their leading bytes
    fn ids(count: usize, crowded: bool, state: &mut u64) -> Vec<Oid> {
        let mut all: Vec<Oid> = (0..count)
            .map(|_| {
                let mut oid = [0u8; 20];
                for eight in oid.chunks_mut(8) {
                    let word = noise(state).to_le_bytes();
                    eight.copy_from_slice(&word[..eight.len()]);
                }
                if crowded && oid[19] % 8 != 0 {
                    let fifth = oid[5] % 3;
                    oid[..5].copy_from_slice(&[0x5a, 0x5a, 0x5a, 0x5a, fifth]);
                }
                oid
            })
            .collect();
        all.sort_unstable();
        all.dedup();
        all
    }

    #[test]
    fn finds_what_a_plain_search_finds() {
        let mut state = 0x243f_6a88_85a3_08d3;
        for crowded in [false, true] {
            for count in [0, 1, 2, 3, 15, 16, 17, 255, 256, 257, 1000, 70_000] {
                let all = ids(count, crowded, &mut state);
                let carried = Carried::new(all.iter().map(|oid| (*oid, kind_of(oid))));
                for oid in &all {
                    assert_eq!(carried.kind(oid), Some(kind_of(oid)), "count {count}");
                    // the ids just below and just above, which are absent unless listed
                    for step in [1u8, 0xff] {
                        let mut near = *oid;
                        near[19] = near[19].wrapping_add(step);
                        let expected = all.binary_search(&near).ok().map(|_| kind_of(&near));
                        assert_eq!(carried.kind(&near), expected, "count {count}");
                    }
                }
                for _ in 0..1000 {
                    let absent = ids(1, crowded, &mut state)[0];
                    let expected = all.binary_search(&absent).ok().map(|_| kind_of(&absent));
                    assert_eq!(carried.kind(&absent), expected);
                }
                assert_eq!(carried.kind(&[0; 20]), None);
                assert_eq!(carried.kind(&[0xff; 20]), None);
            }
        }
    }
}
