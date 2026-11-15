//! Short forms of object ids: resolving a prefix to the object it names,
//! and printing an id with as few digits as keep it apart from the rest.

use std::cell::{Cell, OnceCell};

use alt_git_codec::ObjectId;

use crate::{MapEntry, NativeOdb};

/// Lookups answered by scanning the map before a sorted list is built.
const SCANS: usize = 32;

/// Abbreviates object ids the way git does by default: the length grows
/// with the size of the store, and an id that another object shares that
/// many digits with gets as many more as tell them apart.
pub struct Abbrev<'a> {
    odb: &'a NativeOdb,
    min: usize,
    asked: Cell<usize>,
    sorted: OnceCell<Vec<ObjectId>>,
}

impl NativeOdb {
    /// Resolve a short hex prefix (≥ 4 hex chars) to the unique git oid
    /// it matches. Returns up to `cap` matches so callers can detect
    /// ambiguity without paying for a full scan; cap-saturated results
    /// are treated as "ambiguous" by callers.
    ///
    /// Empty / too-short prefixes return an empty list. Prefix must be
    /// lowercase hex (the canonical form alt prints); the comparison is
    /// against `entry.git.to_string()` so non-hex chars yield no match.
    pub fn lookup_by_prefix(&self, prefix: &str, cap: usize) -> Vec<&MapEntry> {
        if prefix.len() < 4 || prefix.len() >= 64 {
            return Vec::new();
        }
        if !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            return Vec::new();
        }
        let lower: String = prefix.chars().map(|c| c.to_ascii_lowercase()).collect();
        let mut out = Vec::new();
        for entry in self.map.iter() {
            if entry.git.to_string().starts_with(&lower) {
                out.push(entry);
                if out.len() >= cap {
                    break;
                }
            }
        }
        out
    }

    pub fn abbrev(&self) -> Abbrev<'_> {
        Abbrev {
            odb: self,
            min: Abbrev::default_len(self.len()),
            asked: Cell::new(0),
            sorted: OnceCell::new(),
        }
    }
}

/// How many leading hex digits two ids have in common.
fn shared_digits(a: &ObjectId, b: &ObjectId) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let same = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    match (a.get(same), b.get(same)) {
        (Some(x), Some(y)) if x >> 4 == y >> 4 => same * 2 + 1,
        _ => same * 2,
    }
}

impl Abbrev<'_> {
    /// The digits git starts from in a store of `objects` objects: enough
    /// for ids to be expected to stay apart, and never fewer than seven.
    pub fn default_len(objects: usize) -> usize {
        let bits = (usize::BITS - objects.leading_zeros()) as usize;
        bits.div_ceil(2).max(7)
    }

    /// The default number of digits for this store.
    pub fn min_len(&self) -> usize {
        self.min
    }

    /// `oid` cut to the default length, or to one digit more than it
    /// shares with any other object when that is longer. The id need not
    /// be in the store.
    pub fn of(&self, oid: &ObjectId) -> String {
        let hex = oid.to_string();
        let len = (self.shared_with_another(oid) + 1).max(self.min);
        hex[..len.min(hex.len())].to_owned()
    }

    fn shared_with_another(&self, oid: &ObjectId) -> usize {
        self.asked.set(self.asked.get() + 1);
        if self.asked.get() <= SCANS && self.sorted.get().is_none() {
            return most_shared(self.odb.map.iter().map(|e: &MapEntry| &e.git), oid);
        }
        // many ids to print: sort once, then only the neighbours matter
        let sorted = self.sorted.get_or_init(|| {
            let mut ids: Vec<ObjectId> = self.odb.map.iter().map(|e| e.git).collect();
            ids.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            ids
        });
        most_shared(neighbours(sorted, oid), oid)
    }
}

/// The most leading digits `oid` shares with any other of `ids`.
fn most_shared<'i>(ids: impl IntoIterator<Item = &'i ObjectId>, oid: &ObjectId) -> usize {
    let others = ids.into_iter().filter(|e| *e != oid);
    others.map(|e| shared_digits(e, oid)).max().unwrap_or(0)
}

/// The ids next to where `oid` sorts: no other id shares more with it.
fn neighbours<'s>(sorted: &'s [ObjectId], oid: &ObjectId) -> &'s [ObjectId] {
    let at = sorted.partition_point(|e| e.as_bytes() < oid.as_bytes());
    &sorted[at.saturating_sub(1)..(at + 2).min(sorted.len())]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(hex: &str) -> ObjectId {
        ObjectId::from_hex(format!("{hex:0<40}").as_bytes()).unwrap()
    }

    #[test]
    fn an_id_shares_digits_with_its_nearest_neighbour() {
        let mut ids: Vec<ObjectId> = ["abcdef0123", "abcdef0199", "abcdef7", "1234567"]
            .iter()
            .map(|hex| id(hex))
            .collect();
        ids.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        // by scanning every id, and from the sorted list
        let ways: [&dyn Fn(&ObjectId) -> usize; 2] = [&|oid| most_shared(&ids, oid), &|oid| {
            most_shared(neighbours(&ids, oid), oid)
        }];
        for shared in ways {
            assert_eq!(shared(&id("1234567")), 0);
            assert_eq!(shared(&id("abcdef7")), 6);
            assert_eq!(shared(&id("abcdef0123")), 8);
            assert_eq!(shared(&id("abcdef0199")), 8);
            // an id that is not in the list is kept apart from those in it
            assert_eq!(shared(&id("abcdef01ff")), 8);
            assert_eq!(shared(&id("abcdef0")), 7);
            assert_eq!(shared(&id("ff")), 0);
        }
    }

    #[test]
    fn the_starting_length_grows_with_the_store() {
        assert_eq!(Abbrev::default_len(0), 7);
        // the cargo and git repositories, where git prints 9 and 10 digits
        assert_eq!(Abbrev::default_len(185_011), 9);
        assert_eq!(Abbrev::default_len(402_230), 10);
    }
}
