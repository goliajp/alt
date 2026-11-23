//! Whether the tree of a commit a push adds is held whole: the tree, the
//! trees under it and the blobs they name.
//!
//! What the nearest commits under it that the refs reach have at the
//! same path is not looked at: their trees were whole when they came in.
//! So above history that is served a tree is read where it differs from
//! there, and a blob is asked for where that history has another or
//! none. Nothing is left out for being in the tree of a commit the push
//! adds as well: a tree found whole is not read again, and that alone
//! keeps each tree of the push to one read.
//!
//! A tree of the history served is kept as it was read, so that it is
//! read once whatever the number of trees and of parents it is looked up
//! for, while those kept fit [`BASES`]. A push names the commits it is
//! on top of, as many as it likes, and a tree of it stands in the place
//! of one tree of each: what is read of them is bounded by the
//! repository, not by the push. Past the limit the trees kept are let go
//! of and read again when they come up; a tree of the push is held
//! against one of them at a time, and the trees in the place of a
//! directory are found when that directory is come to.

use std::collections::HashMap;
use std::rc::Rc;

use alt_git_codec::{ObjectId, ObjectKind, Tree, TreeEntry};
use alt_hash::{KeyedIdMap, KeyedIdSet};

use super::{Res, Source};

/// The bytes of trees of the history served that are kept, as they are
/// counted here: about what they take in memory.
pub(super) const BASES: usize = 64 << 20;

/// What a tree has at each name: whether it is a directory, and its
/// object.
type Places = HashMap<Vec<u8>, (bool, ObjectId)>;

pub(super) struct Trees {
    /// trees found whole
    whole: KeyedIdSet<ObjectId>,
    /// the trees and blobs found outside what the refs reach
    pub(super) added: KeyedIdSet<ObjectId>,
    /// the trees history the refs reach has, as they were read: `None`
    /// for one that does not read
    bases: KeyedIdMap<ObjectId, Option<Rc<Places>>>,
    /// what `bases` counts for, the most it ever did, and its limit
    kept: usize,
    most: usize,
    limit: usize,
}

/// A directory of a tree of the push that waits to be looked at: its
/// tree, and where the trees in its place are found, the name it has
/// under the trees that are in the place of the tree it is in.
type Waiting = (ObjectId, Rc<Vec<ObjectId>>, Option<Vec<u8>>);

/// The entries of the tree `oid`; `None` for what is not held, or does
/// not read as a tree.
fn entries(source: &Source<'_>, oid: &ObjectId) -> Res<Option<Tree>> {
    let Some(raw) = (source.get)(oid)? else {
        return Ok(None);
    };
    if raw.kind != ObjectKind::Tree {
        return Ok(None);
    }
    Ok(Tree::parse(&raw.data, oid.algo()).ok())
}

/// An entry by its name: whether it is a directory, and its object.
fn place(entry: &TreeEntry) -> (Vec<u8>, (bool, ObjectId)) {
    (entry.name.to_vec(), (entry.mode.is_tree(), entry.oid))
}

/// What a tree kept counts for.
fn cost(places: &Places) -> usize {
    const EACH: usize = 96;
    EACH + places.keys().map(|name| EACH + name.len()).sum::<usize>()
}

impl Trees {
    /// With trees of the history served kept up to `limit` bytes.
    pub(super) fn holding(limit: usize) -> Self {
        Trees {
            whole: KeyedIdSet::default(),
            added: KeyedIdSet::default(),
            bases: KeyedIdMap::default(),
            kept: 0,
            most: 0,
            limit,
        }
    }

    /// The tree `base` of the history served; `None` where it does not
    /// read, and it has nothing in its place then. It is kept while the
    /// trees kept fit their limit: all of them are let go of once one
    /// more does not, and one that is larger than the limit is not kept.
    fn base(&mut self, source: &Source<'_>, base: &ObjectId) -> Res<Option<Rc<Places>>> {
        if let Some(kept) = self.bases.get(base) {
            return Ok(kept.clone());
        }
        let tree = entries(source, base)?;
        let places: Option<Rc<Places>> =
            tree.map(|tree| Rc::new(tree.entries.iter().map(place).collect()));
        let cost = places.as_deref().map_or(0, cost);
        if self.kept + cost > self.limit {
            self.bases.clear();
            self.kept = 0;
        }
        if cost <= self.limit {
            self.bases.insert(*base, places.clone());
            self.kept += cost;
            self.most = self.most.max(self.kept);
        }
        Ok(places)
    }

    /// The trees that `bases` have at the directory `name`.
    fn under(
        &mut self,
        source: &Source<'_>,
        bases: &[ObjectId],
        name: &[u8],
    ) -> Res<Vec<ObjectId>> {
        let mut under = Vec::new();
        for base in bases {
            let places = self.base(source, base)?;
            if let Some((true, oid)) = places.as_deref().and_then(|places| places.get(name)) {
                under.push(*oid);
            }
        }
        under.sort_unstable();
        under.dedup();
        Ok(under)
    }

    /// Whether each of `files`, of a tree of the push, is a blob that
    /// one of `bases` has under the same name, or one that is held:
    /// those are put among `blobs`.
    fn files_held(
        &mut self,
        source: &Source<'_>,
        files: &[&TreeEntry],
        bases: &[ObjectId],
        blobs: &mut KeyedIdSet<ObjectId>,
    ) -> Res<bool> {
        let mut theirs = vec![false; files.len()];
        for base in bases.iter().filter(|_| !files.is_empty()) {
            let Some(places) = self.base(source, base)? else {
                continue;
            };
            for (file, theirs) in files.iter().zip(&mut theirs) {
                *theirs |= places.get(&file.name[..]) == Some(&(false, file.oid));
            }
        }
        for (file, theirs) in files.iter().zip(theirs) {
            if !theirs
                && !self.added.contains(&file.oid)
                && blobs.insert(file.oid)
                && !(source.holds)(&file.oid)?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Whether the tree `root` is held whole, `bases` being the trees
    /// that history the refs reach has in its place.
    pub(super) fn whole(
        &mut self,
        source: &Source<'_>,
        root: ObjectId,
        bases: &[ObjectId],
    ) -> Res<bool> {
        // kept apart until the whole tree is found: one that lacks an
        // object vouches for none of its parts
        let mut trees = KeyedIdSet::default();
        let mut blobs = KeyedIdSet::default();
        let mut stack: Vec<Waiting> = vec![(root, Rc::new(bases.to_vec()), None)];
        while let Some((oid, above, name)) = stack.pop() {
            if self.whole.contains(&oid) || trees.contains(&oid) {
                continue;
            }
            let bases = match name {
                Some(name) => Rc::new(self.under(source, &above, &name)?),
                None => above,
            };
            if bases.contains(&oid) {
                continue;
            }
            trees.insert(oid);
            let Some(tree) = entries(source, &oid)? else {
                return Ok(false);
            };
            // a commit of another repository is none of this one's
            let (dirs, files): (Vec<&TreeEntry>, Vec<&TreeEntry>) = (tree.entries.iter())
                .filter(|entry| !entry.mode.is_gitlink())
                .partition(|entry| entry.mode.is_tree());
            if !self.files_held(source, &files, &bases, &mut blobs)? {
                return Ok(false);
            }
            for dir in dirs {
                stack.push((dir.oid, Rc::clone(&bases), Some(dir.name.to_vec())));
            }
        }
        self.whole.extend(trees.iter().copied());
        self.added.extend(trees);
        self.added.extend(blobs);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_git_codec::{HashAlgo, RawObject};

    /// A tree of `files` files of its own, each under the name every
    /// such tree has it under.
    fn tree(held: &mut HashMap<ObjectId, RawObject>, of: usize, files: usize) -> ObjectId {
        let mut data = Vec::new();
        for n in 0..files {
            let blob = format!("the file {n} of the tree {of}");
            let blob = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, blob.as_bytes());
            data.extend_from_slice(format!("100644 file-{n:03}\0").as_bytes());
            data.extend_from_slice(blob.as_bytes());
        }
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &data);
        let kind = ObjectKind::Tree;
        held.insert(oid, RawObject { kind, data });
        oid
    }

    /// A tree of the push in the place of a hundred trees of the history
    /// served, with a file that none of them has: every one of them is
    /// read, and no more of them are kept than fit.
    #[test]
    fn the_trees_kept_of_the_history_served_fit_their_limit() {
        let mut held = HashMap::new();
        let bases: Vec<ObjectId> = (0..100).map(|n| tree(&mut held, n, 50)).collect();
        let root = tree(&mut held, 1000, 50);
        let reads = std::cell::Cell::new(0);
        let get = |oid: &ObjectId| -> Res<Option<RawObject>> {
            reads.set(reads.get() + 1);
            Ok(held.get(oid).cloned())
        };
        let holds = |_: &ObjectId| -> Res<bool> { Ok(true) };
        let fails = |_: &ObjectId| -> Res<Option<String>> { Ok(None) };
        let source = Source {
            get: &get,
            commit: &|_| Ok(None),
            holds: &holds,
            had: &holds,
            fails: &fails,
        };
        let one = cost(
            &entries(&source, &bases[0])
                .unwrap()
                .unwrap()
                .entries
                .iter()
                .map(place)
                .collect(),
        );
        for (limit, fit) in [(BASES, 100), (10 * one, 10), (one - 1, 0)] {
            let mut trees = Trees::holding(limit);
            reads.set(0);
            assert!(trees.whole(&source, root, &bases).unwrap());
            assert_eq!(reads.get(), 101, "within {limit}");
            assert_eq!(trees.most, fit * one, "within {limit}");
            assert_eq!(trees.added.len(), 51, "within {limit}");
            // looked at again: where trees were let go of, the first of
            // them to come up again takes the place of those still kept
            trees.whole = KeyedIdSet::default();
            assert!(trees.whole(&source, root, &bases).unwrap());
            let again = if fit == 100 { 0 } else { 100 };
            assert_eq!(reads.get(), 101 + 1 + again, "within {limit}");
        }
    }
}
