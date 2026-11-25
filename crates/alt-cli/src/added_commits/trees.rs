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
//! repository, not by the push. Past the limit the trees kept longest
//! without being looked at are let go of, and read again when they come
//! up; a tree of the push is held against one of them at a time.
//!
//! The trees in the place of a directory are found when that directory is
//! come to, and for the directories of one tree together: each tree in the
//! place of that tree is looked at once for all of them, so that trees
//! which do not fit are read once for a tree of the push and not once for
//! each of its directories. What is found waits with the directories, and
//! is held to [`PLACES`]: past that the directories of a tree are taken
//! fewer at a time.
//!
//! A push can be made so that what it stands in the place of never fits
//! and is read over and over: a merge of many commits whose trees are
//! large, with many directories of its own. All that is read of the
//! history served, the first time or again, is counted, and a push that
//! has [`READ`] times what may be kept read for it is refused.

use std::rc::Rc;

use alt_git_codec::{ObjectId, TreeEntry};
use alt_hash::KeyedIdSet;

use super::{Res, Source};
use kept::{Bases, entries};

mod kept;
#[cfg(test)]
mod tests;

/// The bytes of trees of the history served that are kept, as they are
/// counted here: about what they take in memory.
pub(super) const BASES: usize = 64 << 20;
/// How many times that may be read of the history served for one push.
const READ: usize = 64;
/// How many trees are held as the ones in the place of directories that
/// wait, all those directories together.
const PLACES: usize = 1 << 20;
/// What a push is refused with when more would have to be read.
pub const COSTLY: &str = "the trees under the commits this push is on top of are read too often";

pub(super) struct Trees {
    /// trees found whole
    whole: KeyedIdSet<ObjectId>,
    /// the trees and blobs found outside what the refs reach
    pub(super) added: KeyedIdSet<ObjectId>,
    /// the trees of the history served
    served: Bases,
    /// how many trees may be held for the directories that wait
    places: usize,
}

/// A directory of a tree of the push that waits to be looked at.
enum Waiting {
    /// its tree, and the trees in its place
    Placed(ObjectId, Rc<Vec<ObjectId>>),
    /// its tree, the trees in the place of the tree it is in, and the name
    /// it has there
    Named(ObjectId, Rc<Vec<ObjectId>>, Vec<u8>),
}

impl Trees {
    /// With trees of the history served kept up to `limit` bytes, and
    /// [`READ`] times that read at most.
    pub(super) fn holding(limit: usize) -> Self {
        Self::reading(limit, limit.saturating_mul(READ))
    }

    fn reading(limit: usize, budget: usize) -> Self {
        Trees {
            whole: KeyedIdSet::default(),
            added: KeyedIdSet::default(),
            served: Bases::reading(limit, budget),
            places: PLACES,
        }
    }

    /// Why a tree was not found whole.
    pub(super) fn lack(&self) -> &'static str {
        match self.served.costly {
            true => COSTLY,
            false => super::MISSING,
        }
    }

    /// The trees that `bases` have at each of the directories `names`, each
    /// of `bases` looked at once.
    fn under(
        &mut self,
        source: &Source<'_>,
        bases: &[ObjectId],
        names: &[Vec<u8>],
    ) -> Res<Vec<Vec<ObjectId>>> {
        let mut under = vec![Vec::new(); names.len()];
        for base in bases {
            let Some(places) = self.served.of(source, base)? else {
                continue;
            };
            for (name, under) in names.iter().zip(&mut under) {
                if let Some((true, oid)) = places.get(name) {
                    under.push(*oid);
                }
            }
        }
        for under in &mut under {
            under.sort_unstable();
            under.dedup();
        }
        Ok(under)
    }

    /// Finds the trees in the place of the directory `first` and of those
    /// that wait on top of `stack` under the same trees, as many of them as
    /// the trees that may be held leave room for, and puts them back with
    /// what was found. `held` is how many trees are held already.
    fn place(
        &mut self,
        source: &Source<'_>,
        first: (ObjectId, Vec<u8>),
        above: &Rc<Vec<ObjectId>>,
        stack: &mut Vec<Waiting>,
        held: &mut usize,
    ) -> Res<()> {
        let room = self.places.saturating_sub(*held) / above.len().max(1);
        let (mut trees, mut names) = (vec![first.0], vec![first.1]);
        while trees.len() < room
            && let Some(Waiting::Named(_, theirs, _)) = stack.last()
            && Rc::ptr_eq(theirs, above)
        {
            let Some(Waiting::Named(oid, _, name)) = stack.pop() else {
                unreachable!("it was looked at")
            };
            trees.push(oid);
            names.push(name);
        }
        let under = self.under(source, above, &names)?;
        // back in the order they waited in
        for (oid, under) in trees.into_iter().zip(under).rev() {
            *held += under.len();
            stack.push(Waiting::Placed(oid, Rc::new(under)));
        }
        Ok(())
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
            let Some(places) = self.served.of(source, base)? else {
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
        let mut held = bases.len();
        let mut stack = vec![Waiting::Placed(root, Rc::new(bases.to_vec()))];
        while let Some(waiting) = stack.pop() {
            let (oid, bases) = match waiting {
                Waiting::Placed(oid, bases) => {
                    held -= bases.len();
                    (oid, bases)
                }
                Waiting::Named(oid, above, name) => {
                    if !self.whole.contains(&oid) && !trees.contains(&oid) {
                        self.place(source, (oid, name), &above, &mut stack, &mut held)?;
                    }
                    continue;
                }
            };
            if self.whole.contains(&oid) || trees.contains(&oid) || bases.contains(&oid) {
                continue;
            }
            trees.insert(oid);
            let Some(tree) = entries(source, &oid)? else {
                return Ok(false);
            };
            if self.served.costly {
                return Ok(false);
            }
            // a commit of another repository is none of this one's
            let (dirs, files): (Vec<&TreeEntry>, Vec<&TreeEntry>) = (tree.entries.iter())
                .filter(|entry| !entry.mode.is_gitlink())
                .partition(|entry| entry.mode.is_tree());
            if !self.files_held(source, &files, &bases, &mut blobs)? {
                return Ok(false);
            }
            for dir in dirs {
                stack.push(Waiting::Named(
                    dir.oid,
                    Rc::clone(&bases),
                    dir.name.to_vec(),
                ));
            }
        }
        // what was not read of the history served vouches for nothing
        if self.served.costly {
            return Ok(false);
        }
        self.whole.extend(trees.iter().copied());
        self.added.extend(trees);
        self.added.extend(blobs);
        Ok(true)
    }
}
