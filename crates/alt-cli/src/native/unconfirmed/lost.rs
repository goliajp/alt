//! What names an object that goes when records naming content which is
//! not stored are taken out of the object store.

use std::collections::HashSet;

use alt_git_codec::{Commit, ObjectKind, Tag, Tree};

use super::*;

/// Below the `.alt` directory: the ids of objects taken out of the store
/// though commits name them, one to a line.
const COMMITTED_LOST: &str = "objects.lost";

/// An object a ref leads to, with the first place found that names it.
pub(crate) struct Reached {
    pub(crate) oid: ObjectId,
    /// `blob`, `tree`, `commit`, or `object` where nothing tells which
    pub(crate) kind: &'static str,
    /// the ref, the tag or the commit that names it, and how
    pub(crate) by: String,
    /// whether it is what the ref itself points at
    pub(crate) tip: bool,
}

/// A walk from refs through tags, commits and trees that stops at every
/// object `wanted` picks. An object is looked at once, however many
/// commits and trees name it.
struct Walk<'a> {
    store: &'a Store,
    wanted: &'a dyn Fn(&ObjectId) -> Res<bool>,
    seen: alt_hash::IdSet<ObjectId>,
    found: Vec<Reached>,
}

impl Walk<'_> {
    /// Whether `oid` is to be walked into: it was not looked at before
    /// and is not one of the wanted, which is kept with `by`.
    fn enter(
        &mut self,
        oid: ObjectId,
        kind: &'static str,
        by: impl FnOnce() -> String,
    ) -> Res<bool> {
        if !self.seen.insert(oid) {
            return Ok(false);
        }
        if !(self.wanted)(&oid)? {
            return Ok(true);
        }
        let (by, tip) = (by(), false);
        self.found.push(Reached { oid, kind, by, tip });
        Ok(false)
    }

    /// Walks what `tip`, entered already, leads to; `from` is the ref.
    fn below(&mut self, tip: ObjectId, from: &str) -> Res<()> {
        let mut commits = vec![tip];
        while let Some(oid) = commits.pop() {
            let Some(object) = self.store.odb.get(&oid)? else {
                continue;
            };
            match object.kind {
                ObjectKind::Tag => {
                    let Some(target) = Tag::parse(&object.data)?.object() else {
                        continue;
                    };
                    let by = || format!("tag {oid}, reached from {from}, names it");
                    if self.enter(target, "object", by)? {
                        commits.push(target);
                    }
                }
                ObjectKind::Commit => {
                    let commit = Commit::parse(&object.data)?;
                    let by = format!("commit {oid}, reached from {from},");
                    for parent in commit.parents() {
                        if self.enter(parent, "commit", || format!("{by} has it as a parent"))? {
                            commits.push(parent);
                        }
                    }
                    if let Some(tree) = commit.tree()
                        && self.enter(tree, "tree", || format!("{by} names it at /"))?
                    {
                        self.tree(tree, &by)?;
                    }
                }
                ObjectKind::Tree | ObjectKind::Blob => {}
            }
        }
        Ok(())
    }

    /// Walks the tree `root`, entered already, of the commit `by` tells.
    fn tree(&mut self, root: ObjectId, by: &str) -> Res<()> {
        let algo = self.store.algo()?;
        let mut trees = vec![(root, String::new())];
        while let Some((tree, at)) = trees.pop() {
            let Some(object) = self.store.odb.get(&tree)? else {
                continue;
            };
            for entry in Tree::parse(&object.data, algo)?.entries {
                let path = || format!("{by} names it at {at}{}", entry.name);
                if entry.mode.is_tree() {
                    if self.enter(entry.oid, "tree", || path() + "/")? {
                        trees.push((entry.oid, format!("{at}{}/", entry.name)));
                    }
                } else if !entry.mode.is_gitlink() {
                    self.enter(entry.oid, "blob", path)?;
                }
            }
        }
        Ok(())
    }
}

impl Store {
    /// The objects `wanted` picks among those the refs lead to, as the
    /// refs were before the last `dropped` operations of the log: what a
    /// ref points at, what a tag names, the parents and the tree of a
    /// commit, the entries of its trees. Nothing below a picked object
    /// is looked at.
    pub(crate) fn reaching(
        &self,
        dropped: usize,
        wanted: &dyn Fn(&ObjectId) -> Res<bool>,
    ) -> Res<Vec<Reached>> {
        let (seen, found) = (alt_hash::IdSet::default(), Vec::new());
        let mut walk = Walk {
            store: self,
            wanted,
            seen,
            found,
        };
        for (name, target) in self.refs.refs_before(dropped)? {
            let RefTarget::Oid(tip) = target else {
                continue;
            };
            let found = walk.found.len();
            if walk.enter(tip, "object", || format!("ref {name} points at it"))? {
                walk.below(tip, &name)?;
            } else if let Some(reached) = walk.found.get_mut(found) {
                reached.tip = true;
            }
        }
        Ok(walk.found)
    }

    /// Keeps the ids `gone` in `objects.lost`, with those kept there
    /// before, synced: objects about to be taken out of the store though
    /// commits name them. An index that has a file as such a commit has
    /// it then names a blob that is not stored, which nothing else tells
    /// an add that looks at what is staged over HEAD.
    pub(crate) fn keep_committed_lost(&self, gone: &HashSet<ObjectId>) -> Res<()> {
        let mut kept: Vec<ObjectId> = self.committed_lost()?.into_iter().collect();
        kept.extend(gone);
        kept.sort();
        kept.dedup();
        let lines: String = kept.iter().map(|oid| format!("{oid}\n")).collect();
        Ok(crate::durable::write_state(
            &self.alt_dir.join(COMMITTED_LOST),
            lines.as_bytes(),
        )?)
    }

    /// The objects kept by [`Self::keep_committed_lost`] that the store
    /// still does not hold. Once it holds them all again the list goes.
    pub(crate) fn committed_lost(&self) -> Res<HashSet<ObjectId>> {
        let lost = self.kept_lost()?;
        if lost.as_ref().is_some_and(|lost| lost.is_empty()) {
            let _ = std::fs::remove_file(self.alt_dir.join(COMMITTED_LOST));
        }
        Ok(lost.unwrap_or_default())
    }

    /// The same where a list is kept, and no file is changed.
    pub(crate) fn kept_lost(&self) -> Res<Option<HashSet<ObjectId>>> {
        let path = self.alt_dir.join(COMMITTED_LOST);
        let kept = match std::fs::read(&path) {
            Ok(kept) => kept,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut lost = HashSet::new();
        for line in kept.split(|byte| *byte == b'\n').filter(|l| !l.is_empty()) {
            let oid = ObjectId::from_hex(line)
                .map_err(|_| format!("{} holds a line that is no object id", path.display()))?;
            if !self.odb.contains(&oid)? {
                lost.insert(oid);
            }
        }
        Ok(Some(lost))
    }
}
