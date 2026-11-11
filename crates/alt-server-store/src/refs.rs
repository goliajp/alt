//! A repository's references as one read returns them: every name in one buffer,
//! ordered by bytes, so a repository with many references costs two allocations.

use crate::{Error, Result};
use alt_git_codec::ObjectId;
use std::collections::BTreeMap;

/// one row per reference as `<hex object id> <name>`, rows separated by a line feed;
/// a reference name holds neither byte
pub(crate) const REFS_TEXT: &str =
    "SELECT string_agg(oid||' '||name,E'\\n') FROM alt_refs WHERE repository=$1";

fn lines(text: &str) -> impl Iterator<Item = Result<(&str, ObjectId)>> {
    text.split('\n').map(|line| {
        let (oid, name) = line
            .split_once(' ')
            .ok_or(Error::Integrity("stored reference"))?;
        let oid = ObjectId::from_hex(oid.as_bytes())
            .map_err(|_| Error::Integrity("stored reference object id"))?;
        Ok((name, oid))
    })
}

/// the map the publication rules read, from the aggregated text of a repository's rows
pub(crate) fn map(text: Option<&str>) -> Result<BTreeMap<String, ObjectId>> {
    let Some(text) = text else {
        return Ok(BTreeMap::new());
    };
    let mut pairs = Vec::with_capacity(text.len() / 56);
    for line in lines(text) {
        let (name, oid) = line?;
        pairs.push((name.to_owned(), oid));
    }
    // sorted input is built into the tree in one pass
    pairs.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(pairs.into_iter().collect())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Refs {
    names: String,
    /// where each name ends in `names`, with its target, ordered by name
    entries: Vec<(u32, ObjectId)>,
}

impl Refs {
    pub(crate) fn parse(text: Option<&str>) -> Result<Self> {
        let Some(text) = text else {
            return Ok(Self::default());
        };
        let mut parsed = Vec::with_capacity(text.len() / 56);
        for line in lines(text) {
            parsed.push(line?);
        }
        if !parsed.is_sorted_by(|a, b| a.0 < b.0) {
            parsed.sort_unstable_by(|a, b| a.0.cmp(b.0));
        }
        Ok(Self::from_sorted(
            parsed.len(),
            text.len(),
            parsed.into_iter(),
        ))
    }

    fn from_sorted<'a>(
        count: usize,
        bytes: usize,
        sorted: impl Iterator<Item = (&'a str, ObjectId)>,
    ) -> Self {
        let mut names = String::with_capacity(bytes);
        let mut entries = Vec::with_capacity(count);
        for (name, oid) in sorted {
            names.push_str(name);
            entries.push((names.len() as u32, oid));
        }
        Self { names, entries }
    }

    fn name(&self, index: usize) -> &str {
        let start = match index {
            0 => 0,
            index => self.entries[index - 1].0 as usize,
        };
        &self.names[start..self.entries[index].0 as usize]
    }

    pub fn get(&self, name: &str) -> Option<ObjectId> {
        let (mut low, mut high) = (0, self.entries.len());
        while low < high {
            let middle = low + (high - low) / 2;
            match self.name(middle).cmp(name) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => return Some(self.entries[middle].1),
            }
        }
        None
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// in byte order of the names
    pub fn iter(&self) -> impl Iterator<Item = (&str, ObjectId)> {
        (0..self.entries.len()).map(|index| (self.name(index), self.entries[index].1))
    }

    pub fn values(&self) -> impl Iterator<Item = ObjectId> {
        self.entries.iter().map(|entry| entry.1)
    }
}

impl From<&BTreeMap<String, ObjectId>> for Refs {
    fn from(map: &BTreeMap<String, ObjectId>) -> Self {
        Self::from_sorted(
            map.len(),
            map.keys().map(String::len).sum(),
            map.iter().map(|(name, oid)| (name.as_str(), *oid)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    fn oid(hex: &str) -> ObjectId {
        ObjectId::from_hex(hex.as_bytes()).unwrap()
    }

    #[test]
    fn rows_in_any_order_are_found_by_name_and_listed_in_byte_order() {
        let text =
            format!("{A} refs/heads/main\n{B} refs/heads/Z\n{A} refs/heads/a/b\n{B} refs/heads/a");
        let refs = Refs::parse(Some(&text)).unwrap();
        assert_eq!(refs.len(), 4);
        assert_eq!(refs.get("refs/heads/main"), Some(oid(A)));
        assert_eq!(refs.get("refs/heads/a"), Some(oid(B)));
        assert_eq!(refs.get("refs/heads/a/b"), Some(oid(A)));
        assert_eq!(refs.get("refs/heads/Z"), Some(oid(B)));
        for missing in [
            "",
            "refs/heads",
            "refs/heads/mai",
            "refs/heads/main2",
            "zzz",
        ] {
            assert_eq!(refs.get(missing), None, "{missing}");
        }
        let listed: Vec<_> = refs.iter().collect();
        let map = map(Some(&text)).unwrap();
        let expected: Vec<_> = map
            .iter()
            .map(|(name, oid)| (name.as_str(), *oid))
            .collect();
        assert_eq!(listed, expected);
        assert_eq!(Refs::from(&map), refs);
        assert_eq!(refs.values().filter(|value| *value == oid(A)).count(), 2);
    }

    #[test]
    fn no_rows_is_an_empty_set_and_a_damaged_row_is_refused() {
        let refs = Refs::parse(None).unwrap();
        assert!(refs.is_empty());
        assert_eq!(refs.get("refs/heads/main"), None);
        assert!(map(None).unwrap().is_empty());
        for text in [
            "refs/heads/main".to_owned(),
            format!("{A}refs/heads/main"),
            format!("{} refs/heads/main", &A[1..]),
            format!("{A} refs/heads/main\n"),
            format!("g{} refs/heads/main", &A[1..]),
        ] {
            assert!(
                matches!(Refs::parse(Some(&text)), Err(Error::Integrity(_))),
                "{text}"
            );
            assert!(
                matches!(map(Some(&text)), Err(Error::Integrity(_))),
                "{text}"
            );
        }
    }
}
