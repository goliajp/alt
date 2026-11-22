//! Whether the tree of a commit a push adds is held whole: the tree, the
//! trees under it and the blobs they name.
//!
//! What the nearest commits under it that the refs reach have at the
//! same path is not looked at: their trees were whole when they came in.
//! So above history that is served a tree is read where it differs from
//! there, and a blob is asked for where that history has another or
//! none. Nothing is left out for being in the tree of a commit the push
//! adds as well: a tree found whole is not read again, and that alone
//! keeps each tree of the push to one read. A tree of the history served
//! is kept as it was read, so it is read once as well, whatever the
//! number of trees and of parents it is looked up for.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use alt_git_codec::{ObjectId, ObjectKind, Tree, TreeEntry};

use super::{Res, Source};

/// What a tree has at each name: whether it is a directory, and its
/// object.
type Places = HashMap<Vec<u8>, (bool, ObjectId)>;

#[derive(Default)]
pub(super) struct Trees {
    /// trees found whole
    whole: HashSet<ObjectId>,
    /// the trees and blobs found outside what the refs reach
    pub(super) added: HashSet<ObjectId>,
    /// the trees history the refs reach has, as they were read: `None`
    /// for one that does not read
    bases: HashMap<ObjectId, Option<Rc<Places>>>,
}

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

impl Trees {
    /// Those of `bases` that read: one that does not has nothing in its
    /// place. Each is read once, however many trees of the push stand in
    /// its place, and however many commits under the push have it.
    fn read(&mut self, source: &Source<'_>, bases: &[ObjectId]) -> Res<Vec<Rc<Places>>> {
        let mut read = Vec::new();
        for base in bases {
            if !self.bases.contains_key(base) {
                let tree = entries(source, base)?;
                let places = tree.map(|tree| Rc::new(tree.entries.iter().map(place).collect()));
                self.bases.insert(*base, places);
            }
            read.extend(self.bases[base].clone());
        }
        Ok(read)
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
        let mut trees = HashSet::new();
        let mut blobs = HashSet::new();
        let mut stack = vec![(root, bases.to_vec())];
        while let Some((oid, bases)) = stack.pop() {
            if bases.contains(&oid) || self.whole.contains(&oid) || !trees.insert(oid) {
                continue;
            }
            let Some(tree) = entries(source, &oid)? else {
                return Ok(false);
            };
            let theirs = self.read(source, &bases)?;
            for entry in &tree.entries {
                // a commit of another repository
                if entry.mode.is_gitlink() {
                    continue;
                }
                let is_tree = entry.mode.is_tree();
                let mut under: Vec<ObjectId> = theirs
                    .iter()
                    .filter_map(|base| base.get(&entry.name[..]))
                    .filter(|(tree, _)| *tree == is_tree)
                    .map(|(_, oid)| *oid)
                    .collect();
                under.sort_unstable();
                under.dedup();
                if is_tree {
                    stack.push((entry.oid, under));
                } else if !under.contains(&entry.oid)
                    && !self.added.contains(&entry.oid)
                    && blobs.insert(entry.oid)
                    && !(source.holds)(&entry.oid)?
                {
                    return Ok(false);
                }
            }
        }
        self.whole.extend(trees.iter().copied());
        self.added.extend(trees);
        self.added.extend(blobs);
        Ok(true)
    }
}
