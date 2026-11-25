//! The trees of the history served, kept as they were read.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use alt_git_codec::{ObjectId, ObjectKind, Tree, TreeEntry};
use alt_hash::KeyedIdMap;

use super::super::{Res, Source};

/// What a tree has at each name: whether it is a directory, and its
/// object.
pub(super) type Places = HashMap<Vec<u8>, (bool, ObjectId)>;

pub(super) struct Bases {
    /// the trees history the refs reach has, as they were read: `None`
    /// for one that does not read
    pub(super) bases: KeyedIdMap<ObjectId, Kept>,
    /// the trees kept by when they were looked at, the longest ago
    /// first; one looked at again since has a later entry as well
    pub(super) looked: VecDeque<(u64, ObjectId)>,
    clock: u64,
    /// what `bases` counts for, the most it ever did, and its limit
    pub(super) kept: usize,
    pub(super) most: usize,
    limit: usize,
    /// what was read of the history served so far, as it is counted,
    /// and the most a push may have read
    read: usize,
    budget: usize,
    /// set once more would have to be read than a push may have read
    pub(super) costly: bool,
}

/// A tree kept: when it was last looked at, and what it counts for.
pub(super) struct Kept {
    places: Option<Rc<Places>>,
    looked: u64,
    cost: usize,
}

/// The entries of the tree `oid`; `None` for what is not held, or does
/// not read as a tree.
pub(super) fn entries(source: &Source<'_>, oid: &ObjectId) -> Res<Option<Tree>> {
    let Some(raw) = (source.get)(oid)? else {
        return Ok(None);
    };
    if raw.kind != ObjectKind::Tree {
        return Ok(None);
    }
    Ok(Tree::parse(&raw.data, oid.algo()).ok())
}

/// An entry by its name: whether it is a directory, and its object.
pub(super) fn place(entry: &TreeEntry) -> (Vec<u8>, (bool, ObjectId)) {
    (entry.name.to_vec(), (entry.mode.is_tree(), entry.oid))
}

/// what a tree kept counts for besides its names, and each of its entries
const EACH: usize = 96;

/// What a tree kept counts for.
pub(super) fn cost(places: &Places) -> usize {
    EACH + places.keys().map(|name| EACH + name.len()).sum::<usize>()
}

impl Bases {
    pub(super) fn reading(limit: usize, budget: usize) -> Self {
        Bases {
            bases: KeyedIdMap::default(),
            looked: VecDeque::new(),
            clock: 0,
            kept: 0,
            most: 0,
            limit,
            read: 0,
            budget,
            costly: false,
        }
    }

    /// The tree `base` of the history served; `None` where it does not
    /// read, and it has nothing in its place then. It is kept while the
    /// trees kept fit their limit: those looked at longest ago are let
    /// go of for it, and one that is larger than the limit is not kept.
    /// Nothing is read once the push is found too costly.
    pub(super) fn of(&mut self, source: &Source<'_>, base: &ObjectId) -> Res<Option<Rc<Places>>> {
        self.clock += 1;
        if let Some(kept) = self.bases.get_mut(base) {
            kept.looked = self.clock;
            let places = kept.places.clone();
            self.looked.push_back((self.clock, *base));
            self.thin();
            return Ok(places);
        }
        if self.costly {
            return Ok(None);
        }
        let tree = entries(source, base)?;
        let places: Option<Rc<Places>> =
            tree.map(|tree| Rc::new(tree.entries.iter().map(place).collect()));
        let cost = places.as_deref().map_or(EACH, cost);
        self.read = self.read.saturating_add(cost);
        self.costly = self.read > self.budget;
        if cost > self.limit {
            return Ok(places);
        }
        while self.kept + cost > self.limit {
            let (looked, oldest) = self.looked.pop_front().expect("what is kept was looked at");
            if self
                .bases
                .get(&oldest)
                .is_some_and(|kept| kept.looked == looked)
            {
                self.kept -= self.bases.remove(&oldest).expect("found").cost;
            }
        }
        let looked = self.clock;
        self.looked.push_back((looked, *base));
        let kept = Kept {
            places: places.clone(),
            looked,
            cost,
        };
        self.bases.insert(*base, kept);
        self.kept += cost;
        self.most = self.most.max(self.kept);
        Ok(places)
    }

    /// Drops the entries of `looked` that a later look made stale, once
    /// they are most of it.
    fn thin(&mut self) {
        if self.looked.len() > 2 * self.bases.len() + 64 {
            let bases = &self.bases;
            let current = |(looked, oid): &(u64, ObjectId)| {
                bases.get(oid).is_some_and(|kept| kept.looked == *looked)
            };
            self.looked.retain(current);
        }
    }
}
