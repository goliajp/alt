//! Which objects a fetch has to send: those reachable from what the client wants and
//! not from what it has.
//!
//! The work follows the size of the difference, not of the repository. Each wanted
//! commit's tree is compared with the tree of its first parent, and only directories
//! that differ are opened. What the client is known to hold is collected along the
//! same paths, from the trees of the commits it has.

use crate::{
    Error, Kind, Oid, OidMap, OidSet, SegmentSet,
    object::{self, Parsed},
    trees::{Beside, Trees, entries},
};
use std::collections::{BinaryHeap, HashMap};

const WANT: u8 = 1;
const HAVE: u8 = 2;

/// A blob of at least this size is worth an exact answer to "does the client hold it":
/// walking the trees the client has costs less than sending the blob again.
const EXACT_FROM: u64 = 256 * 1024;

/// the objects a client asked for and does not have
pub struct Selection {
    pub(crate) objects: Vec<Oid>,
    /// objects the client is known to hold; usable as delta bases of a thin pack
    pub(crate) known: OidSet,
}

impl Selection {
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn objects(&self) -> &[Oid] {
        &self.objects
    }
}

/// adds `bit` to a commit's flags, queueing it on first sight; `open` counts queued commits
/// that are wanted and not known to be held
fn mark(
    flags: &mut OidMap<u8>,
    heap: &mut BinaryHeap<(u32, Oid)>,
    open: &mut usize,
    oid: Oid,
    bit: u8,
    generation: u32,
) {
    let entry = flags.entry(oid).or_insert_with(|| {
        heap.push((generation, oid));
        0
    });
    let before = *entry;
    *entry |= bit;
    match (before, *entry) {
        (0, WANT) => *open += 1,
        (WANT, both) if both & HAVE != 0 => *open -= 1,
        _ => {}
    }
}

/// what the walk has gathered so far
struct Found<'a> {
    trees: Trees<'a>,
    /// candidates in the order found, with whether each is a blob
    objects: Vec<(Oid, bool)>,
    seen: OidSet,
    known: OidSet,
    /// trees whose whole content is in `known`
    walked: OidSet,
    /// comparisons made: a tree and the tree it was compared with
    compared: alt_hash::KeyedIdSet<(Oid, Option<Oid>)>,
}

impl<'a> Found<'a> {
    fn new(set: &'a SegmentSet) -> Self {
        Found {
            trees: Trees::new(set),
            objects: Vec::new(),
            seen: OidSet::default(),
            known: OidSet::default(),
            walked: OidSet::default(),
            compared: Default::default(),
        }
    }

    fn add(&mut self, oid: Oid, blob: bool) -> bool {
        let new = self.seen.insert(oid);
        if new {
            self.objects.push((oid, blob));
        }
        new
    }
}

impl SegmentSet {
    /// Adds what `tree` holds and `base` does not hold at the same path. `held` are the
    /// trees at that path in commits the client has: their entries are recorded as
    /// known for every directory opened, and what they hold in a directory that no
    /// longer exists is recorded whole, so that content moved between directories that
    /// changed is not sent again.
    fn tree_difference(
        &self,
        tree: Oid,
        base: Option<Oid>,
        held: Vec<Oid>,
        found: &mut Found<'_>,
    ) -> Result<(), Error> {
        let mut pending = vec![(tree, base, held)];
        while let Some((tree, base, held)) = pending.pop() {
            if held.contains(&tree) {
                found.known.insert(tree);
                continue;
            }
            if base == Some(tree) {
                continue;
            }
            found.add(tree, false);
            // A comparison leaves what the two trees share to the walk below the
            // base, so a tree is settled per base, not once: a directory that
            // returns to an earlier state is compared with the state between, and
            // that state with it, and neither covers what they share.
            if !found.compared.insert((tree, base)) {
                continue;
            }
            let data = found.trees.read(&tree)?;
            let new = entries(&data)?;
            let old_data = base.map(|base| found.trees.read(&base)).transpose()?;
            let old = old_data.as_deref().map(|data| entries(data)).transpose()?;
            let mut old = Beside::new(old.as_deref().unwrap_or_default());
            // per name, the trees the client has there
            let mut held_below: HashMap<&[u8], Vec<Oid>> = HashMap::new();
            let held_data = held
                .iter()
                .map(|held| found.trees.read(held))
                .collect::<Result<Vec<_>, Error>>()?;
            for data in &held_data {
                for entry in entries(data)? {
                    found.known.insert(entry.oid);
                    if entry.tree {
                        held_below.entry(entry.name).or_default().push(entry.oid);
                    }
                }
            }
            for entry in &new {
                let held = match held_below.is_empty() {
                    true => None,
                    false => held_below.remove(entry.name),
                };
                let before = old.of(entry);
                if before == Some(entry.oid) {
                    continue;
                }
                if entry.tree {
                    pending.push((entry.oid, before, held.unwrap_or_default()));
                } else {
                    found.add(entry.oid, true);
                }
            }
            // directories the client has here that are gone, or replaced by a file
            for gone in held_below.into_values().flatten() {
                self.tree_known(&gone, found)?;
            }
        }
        Ok(())
    }

    /// records `tree` and everything under it as known
    fn tree_known(&self, tree: &Oid, found: &mut Found<'_>) -> Result<(), Error> {
        let mut pending = vec![*tree];
        while let Some(oid) = pending.pop() {
            found.known.insert(oid);
            // a tree can be known without its content having been recorded
            if !found.walked.insert(oid) {
                continue;
            }
            for entry in entries(&found.trees.read(&oid)?)? {
                if entry.tree {
                    pending.push(entry.oid);
                } else {
                    found.known.insert(entry.oid);
                }
            }
        }
        Ok(())
    }

    /// whether the selection would send a large blob, which is when it pays to find out
    /// exactly what the client holds
    fn sends_large_blob(&self, found: &Found<'_>) -> Result<bool, Error> {
        for (oid, blob) in &found.objects {
            if *blob
                && !found.known.contains(oid)
                && self.record(oid)?.ok_or(Error::NotFound(*oid))?.size >= EXACT_FROM
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// objects reachable from `wants` and not from `haves`; unknown haves are ignored
    pub fn select(&self, wants: &[Oid], haves: &[Oid]) -> Result<Selection, Error> {
        let mut found = Found::new(self);
        let mut flags: OidMap<u8> = OidMap::default();
        let mut heap = BinaryHeap::new();
        let mut open = 0usize;
        let mut trees = Vec::new();
        for want in wants {
            // peel tags down to what they name
            let mut oid = *want;
            loop {
                match self.kind(&oid)?.ok_or(Error::NotFound(oid))? {
                    Kind::Commit => {
                        let generation = self
                            .commit(&oid)
                            .ok_or(Error::Corrupt("commit graph"))?
                            .generation;
                        mark(&mut flags, &mut heap, &mut open, oid, WANT, generation);
                        break;
                    }
                    Kind::Tag => {
                        found.add(oid, false);
                        let (kind, data) = self.read(&oid)?;
                        let Parsed::Tag { object, .. } = object::parse(kind, &data)? else {
                            unreachable!()
                        };
                        oid = object;
                    }
                    Kind::Tree => {
                        trees.push(oid);
                        break;
                    }
                    Kind::Blob => {
                        found.add(oid, true);
                        break;
                    }
                }
            }
        }
        for have in haves {
            if let Some(commit) = self.commit(have) {
                mark(
                    &mut flags,
                    &mut heap,
                    &mut open,
                    *have,
                    HAVE,
                    commit.generation,
                );
            }
        }
        // children always carry a higher generation than their parents, so a commit's flags
        // are final when it leaves the heap
        let mut commits = Vec::new();
        let mut boundary = Vec::new();
        while open > 0 {
            let Some((_, oid)) = heap.pop() else { break };
            let state = flags[&oid];
            let commit = self.commit(&oid).ok_or(Error::Corrupt("commit graph"))?;
            if state & HAVE != 0 {
                boundary.push(commit.tree);
            } else {
                open -= 1;
                commits.push((oid, commit.tree, commit.parents.first().copied()));
            }
            for parent in &commit.parents {
                let generation = self
                    .commit(parent)
                    .ok_or(Error::Corrupt("commit graph"))?
                    .generation;
                mark(
                    &mut flags,
                    &mut heap,
                    &mut open,
                    *parent,
                    if state & HAVE != 0 { HAVE } else { WANT },
                    generation,
                );
            }
        }
        for (_, oid) in heap {
            if flags[&oid] & HAVE != 0
                && let Some(commit) = self.commit(&oid)
            {
                boundary.push(commit.tree);
            }
        }
        // A parent is either held by the client or wanted, and then sent in full by this
        // same rule, so whatever a commit shares with its first parent need not be sent.
        boundary.sort_unstable();
        boundary.dedup();
        for (oid, tree, parent) in &commits {
            found.add(*oid, false);
            let base = match parent {
                Some(parent) => Some(self.commit(parent).ok_or(Error::Corrupt("commit graph"))?),
                None => None,
            };
            self.tree_difference(
                *tree,
                base.map(|base| base.tree),
                boundary.clone(),
                &mut found,
            )?;
        }
        for tree in &trees {
            self.tree_difference(*tree, None, boundary.clone(), &mut found)?;
        }
        if !boundary.is_empty() && self.sends_large_blob(&found)? {
            for tree in &boundary {
                self.tree_known(tree, &mut found)?;
            }
        }
        let Found { objects, known, .. } = found;
        let objects = objects
            .into_iter()
            .map(|(oid, _)| oid)
            .filter(|oid| !known.contains(oid))
            .collect();
        Ok(Selection { objects, known })
    }
}
