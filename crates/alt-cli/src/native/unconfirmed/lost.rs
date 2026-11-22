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

/// How a walk came to a tag or a commit: put in words for one it finds.
#[derive(Clone, Copy)]
enum Link {
    /// the ref points at it
    Tip,
    /// the tag names it
    Tag(ObjectId),
    /// the commit has it as a parent
    Parent(ObjectId),
}

impl Link {
    fn told(self, from: &str) -> String {
        match self {
            Link::Tip => format!("ref {from} points at it"),
            Link::Tag(tag) => format!("tag {tag}, reached from {from}, names it"),
            Link::Parent(of) => format!("commit {of}, reached from {from}, has it as a parent"),
        }
    }
}

/// What a tree is walked for: `commit` and the commit it is the tree of,
/// or `tree` and the tree itself where a ref or a tag leads to it.
type Top = (&'static str, ObjectId);

/// A walk from refs through tags, commits and trees that stops at every
/// object `wanted` picks. An object is looked at once, however many
/// commits and trees name it.
struct Walk<'a> {
    store: &'a Store,
    /// asked of an object met for the first time, with whether the walk
    /// goes on to read it, and of one it read that the store has not
    wanted: &'a dyn Fn(&ObjectId, bool) -> Res<bool>,
    /// whether every record of the store was verified before the walk
    verified: bool,
    seen: alt_hash::IdSet<ObjectId>,
    found: Vec<Reached>,
    /// the directories entered of the tree being walked, the root first:
    /// the one each is in and where `names` has its name
    dirs: Vec<(u32, std::ops::Range<usize>)>,
    names: Vec<u8>,
}

impl Walk<'_> {
    /// Whether `oid` is to be walked into: it was not looked at before
    /// and is not one of the wanted, which is kept with `by`.
    fn enter(
        &mut self,
        oid: ObjectId,
        kind: &'static str,
        read: bool,
        by: impl FnOnce(&Self) -> String,
    ) -> Res<bool> {
        if !self.seen.insert(oid) {
            return Ok(false);
        }
        if !(self.wanted)(&oid, read)? {
            return Ok(true);
        }
        let (by, tip) = (by(self), false);
        self.found.push(Reached { oid, kind, by, tip });
        Ok(false)
    }

    /// The object `oid`, entered to be read. One the store does not hold
    /// is asked for as an object that is not read, and kept with `by`
    /// where it is wanted.
    fn read(
        &mut self,
        oid: ObjectId,
        kind: &'static str,
        by: impl FnOnce(&Self) -> String,
    ) -> Res<Option<alt_git_codec::RawObject>> {
        let object = match self.verified {
            true => self.store.odb.get_unverified(&oid)?,
            false => self.store.odb.get(&oid)?,
        };
        if object.is_none() && (self.wanted)(&oid, false)? {
            let (by, tip) = (by(self), false);
            self.found.push(Reached { oid, kind, by, tip });
        }
        Ok(object)
    }

    /// Walks what `tip`, entered already, leads to; `from` is the ref.
    fn below(&mut self, tip: ObjectId, from: &str) -> Res<()> {
        let mut commits = vec![(tip, "object", Link::Tip)];
        while let Some((oid, kind, link)) = commits.pop() {
            let Some(object) = self.read(oid, kind, |_| link.told(from))? else {
                continue;
            };
            match object.kind {
                ObjectKind::Tag => {
                    let Some(target) = Tag::parse(&object.data)?.object() else {
                        continue;
                    };
                    let link = Link::Tag(oid);
                    if self.enter(target, "object", true, |_| link.told(from))? {
                        commits.push((target, "object", link));
                    }
                }
                ObjectKind::Commit => {
                    let commit = Commit::parse(&object.data)?;
                    let link = Link::Parent(oid);
                    for parent in commit.parents() {
                        if self.enter(parent, "commit", true, |_| link.told(from))? {
                            commits.push((parent, "commit", link));
                        }
                    }
                    if let Some(tree) = commit.tree() {
                        self.tree(tree, oid, from)?;
                    }
                }
                // a tree a ref or a tag names, as `v2.6.11-tree` of Linux
                ObjectKind::Tree => self.trees(oid, ("tree", oid), from)?,
                ObjectKind::Blob => {}
            }
        }
        Ok(())
    }

    /// Where the tree walked for `top`, a commit or the tree itself, has
    /// the entry `name` of the directory `dir`, or with no name that
    /// directory itself.
    fn at(&self, top: Top, from: &str, mut dir: usize, name: &[u8]) -> String {
        let mut path = vec![name];
        while dir != 0 {
            let (parent, within) = &self.dirs[dir];
            path.extend([&b"/"[..], &self.names[within.clone()]]);
            dir = *parent as usize;
        }
        if path == [b""] {
            path.push(b"/");
        }
        path.reverse();
        let path = alt_bytes::BString::from(path.concat());
        let (kind, top) = top;
        format!("{kind} {top}, reached from {from}, names it at {path}")
    }

    /// Walks the tree `root` of `commit`, which the ref `from` leads to.
    fn tree(&mut self, root: ObjectId, commit: ObjectId, from: &str) -> Res<()> {
        let top = ("commit", commit);
        match self.enter(root, "tree", true, |walk| walk.at(top, from, 0, b""))? {
            true => self.trees(root, top, from),
            false => Ok(()),
        }
    }

    /// Walks the tree `root`, entered already, and the trees below it.
    fn trees(&mut self, root: ObjectId, top: Top, from: &str) -> Res<()> {
        let algo = self.store.algo()?;
        self.dirs.clear();
        self.names.clear();
        self.dirs.push((0, 0..0));
        let mut trees = vec![(root, 0)];
        while let Some((tree, dir)) = trees.pop() {
            let by = |walk: &Self| walk.at(top, from, dir, b"");
            let Some(object) = self.read(tree, "tree", by)? else {
                continue;
            };
            for entry in Tree::raw_entries(&object.data, algo) {
                let (mode, name, oid) = entry?;
                if mode.is_gitlink() {
                    continue;
                }
                let oid = ObjectId::from_bytes(algo, oid).map_err(|e| e.to_string())?;
                if !mode.is_tree() {
                    self.enter(oid, "blob", false, |walk| walk.at(top, from, dir, name))?;
                } else if self.enter(oid, "tree", true, |walk| {
                    walk.at(top, from, dir, name) + "/"
                })? {
                    let start = self.names.len();
                    self.names.extend_from_slice(name);
                    self.dirs.push((dir as u32, start..self.names.len()));
                    trees.push((oid, self.dirs.len() - 1));
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
    /// commit, the entries of its trees and of a tree a ref or a tag
    /// names. Nothing below a picked object is looked at. `wanted` is
    /// asked of each object once, and told whether the walk reads the
    /// object next; of an object it reads and the store does not hold it
    /// is asked again, told that it does not. With `verified` the walk
    /// hashes nothing it reads: every record of the store was verified
    /// before.
    pub(crate) fn reaching(
        &self,
        dropped: usize,
        verified: bool,
        wanted: &dyn Fn(&ObjectId, bool) -> Res<bool>,
    ) -> Res<Vec<Reached>> {
        let mut walk = Walk {
            store: self,
            wanted,
            verified,
            seen: alt_hash::IdSet::default(),
            found: Vec::new(),
            dirs: Vec::new(),
            names: Vec::new(),
        };
        for (name, target) in self.refs.refs_before(dropped)? {
            let RefTarget::Oid(tip) = target else {
                continue;
            };
            let found = walk.found.len();
            if walk.enter(tip, "object", true, |_| Link::Tip.told(&name))? {
                walk.below(tip, &name)?;
            }
            if let Some(reached) = walk.found.get_mut(found)
                && reached.oid == tip
            {
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
