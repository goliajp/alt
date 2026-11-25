//! What the objects of a pack say of other objects, taken in as each is verified.

use crate::{
    Error, Kind, Led, Oid, SegmentSet,
    format::CommitRow,
    object::{self, Parsed},
    wanted::Wanted,
};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

pub(crate) struct Told<'a> {
    pub wanted: Wanted<'a>,
    parents: AtomicUsize,
    most_parents: usize,
}

impl<'a> Told<'a> {
    /// for a pack of `entries` entries received beside `existing` from one who is
    /// led to `led` of it, whose commits may name `parents_beside` parents beyond
    /// two for each entry
    pub fn new(
        (existing, led): (&'a SegmentSet, Option<&'a Led>),
        entries: usize,
        parents_beside: usize,
    ) -> Self {
        Told {
            wanted: Wanted::new(existing, led, entries),
            parents: AtomicUsize::new(0),
            most_parents: entries.saturating_mul(2).saturating_add(parents_beside),
        }
    }

    /// Takes in what the entry `index`, the object `oid`, refers to. The row of a
    /// commit, without its generation.
    pub fn object(
        &self,
        index: u32,
        oid: &Oid,
        kind: Kind,
        data: &[u8],
    ) -> Result<Option<CommitRow>, Error> {
        let want = |to, kind| self.wanted.want(index, oid, to, kind);
        if kind == Kind::Tree {
            if let Some(wrong) = crate::unsafe_names::found(data) {
                return Err(Error::Unsafe(*oid, wrong));
            }
            object::tree(data, &mut { want })?;
            return Ok(None);
        }
        match object::parse(kind, data)? {
            Parsed::Commit { tree, parents } => {
                let before = self.parents.fetch_add(parents.len(), Relaxed);
                if before.saturating_add(parents.len()) > self.most_parents {
                    return Err(Error::Limit("commit parents"));
                }
                want(tree, Kind::Tree)?;
                parents
                    .iter()
                    .try_for_each(|parent| want(*parent, Kind::Commit))?;
                Ok(Some(CommitRow {
                    oid: *oid,
                    tree,
                    generation: 0,
                    parents,
                }))
            }
            Parsed::Tag { object, kind } => want(object, kind).map(|()| None),
            Parsed::Blob | Parsed::Tree(_) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(parents: usize) -> Vec<u8> {
        let mut commit = format!("tree {}\n", "1".repeat(40));
        for _ in 0..parents {
            commit += &format!("parent {}\n", "2".repeat(40));
        }
        (commit + "\nmessage\n").into_bytes()
    }

    #[test]
    fn the_commits_of_a_pack_name_two_parents_an_entry_and_some_beside() {
        let none = SegmentSet::default();
        // three entries, four parents beside: ten
        let told = Told::new((&none, None), 3, 4);
        let row = told.object(0, &[1; 20], Kind::Commit, &commit(6)).unwrap();
        assert_eq!(row.unwrap().parents.len(), 6);
        told.object(1, &[2; 20], Kind::Commit, &commit(4)).unwrap();
        let refused = told.object(2, &[3; 20], Kind::Commit, &commit(1));
        assert!(
            matches!(refused, Err(Error::Limit("commit parents"))),
            "{:?}",
            refused.map(|_| ())
        );
    }
}
