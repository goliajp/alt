//! Short forms of object ids: resolving a prefix to the object it names,
//! and printing an id with as few digits as keep it apart from the rest.

use std::cell::OnceCell;

use alt_git_codec::ObjectId;

use crate::{MapEntry, NativeOdb};

/// Abbreviates object ids the way git does by default: the length grows
/// with the size of the store, and an id that another object shares that
/// many digits with gets as many more as tell them apart.
pub struct Abbrev<'a> {
    odb: &'a NativeOdb,
    min: usize,
    grouped: OnceCell<Grouped>,
}

/// Every id of the store, those with the same first two bytes next to
/// each other. Two ids that differ there share fewer digits than the
/// shortest abbreviation has, so only an id's own group matters to it.
struct Grouped {
    ids: Vec<ObjectId>,
    /// where the group of each two-byte value begins; one more entry closes the last
    starts: Vec<u32>,
}

fn group_of(oid: &ObjectId) -> usize {
    let b = oid.as_bytes();
    (b[0] as usize) << 8 | b[1] as usize
}

impl Grouped {
    /// One pass to count and one to place: no id is compared with another.
    fn of<'i, I: Iterator<Item = &'i ObjectId>>(ids: impl Fn() -> I) -> Grouped {
        let mut starts = vec![0u32; (1 << 16) + 1];
        for oid in ids() {
            starts[group_of(oid) + 1] += 1;
        }
        for group in 0..1 << 16 {
            starts[group + 1] += starts[group];
        }
        let total = starts[1 << 16] as usize;
        let mut next = starts.clone();
        let mut placed: Vec<Option<ObjectId>> = vec![None; total];
        for oid in ids() {
            let at = &mut next[group_of(oid)];
            placed[*at as usize] = Some(*oid);
            *at += 1;
        }
        Grouped {
            ids: placed.into_iter().flatten().collect(),
            starts,
        }
    }

    fn with(&self, oid: &ObjectId) -> &[ObjectId] {
        let group = group_of(oid);
        &self.ids[self.starts[group] as usize..self.starts[group + 1] as usize]
    }
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
            grouped: OnceCell::new(),
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
        let grouped = self
            .grouped
            .get_or_init(|| Grouped::of(|| self.odb.map.iter().map(|e: &MapEntry| &e.git)));
        most_shared(grouped.with(oid), oid)
    }
}

/// The most leading digits `oid` shares with any other of `ids`.
fn most_shared<'i>(ids: impl IntoIterator<Item = &'i ObjectId>, oid: &ObjectId) -> usize {
    let others = ids.into_iter().filter(|e| *e != oid);
    others.map(|e| shared_digits(e, oid)).max().unwrap_or(0)
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
        let grouped = Grouped::of(|| ids.iter());
        assert_eq!(grouped.ids.len(), ids.len());
        // by scanning every id, and from the id's own group: the same
        // wherever the answer reaches the shortest abbreviation
        let ways: [&dyn Fn(&ObjectId) -> usize; 2] = [&|oid| most_shared(&ids, oid), &|oid| {
            most_shared(grouped.with(oid), oid)
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
