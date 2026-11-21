//! What names an object that goes when records naming content which is
//! not stored are taken out of the object store.

use std::collections::HashSet;

use alt_git_codec::{Commit, ObjectKind, Tag, Tree};

use super::*;

/// Below the `.alt` directory: the ids of objects taken out of the store
/// though commits name them, one to a line.
const COMMITTED_LOST: &str = "objects.lost";

impl Store {
    /// A line for each place a ref leads to one of `lost`: the ref
    /// itself, a tag, the parent or the tree of a commit, a path in the
    /// tree of a commit. A commit or a tree is walked once, so an object
    /// many commits share is told with the first of them that is found.
    pub(crate) fn reaching(&self, lost: &HashSet<ObjectId>) -> Res<Vec<String>> {
        let algo = self.algo()?;
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        for (name, target) in self.refs.refs_before(0)? {
            let RefTarget::Oid(tip) = target else {
                continue;
            };
            if lost.contains(&tip) {
                found.push(format!("ref {name} points at {tip}"));
            }
            let mut commits = vec![tip];
            while let Some(oid) = commits.pop() {
                if lost.contains(&oid) || !seen.insert(oid) {
                    continue;
                }
                let Some(object) = self.odb.get(&oid)? else {
                    continue;
                };
                match object.kind {
                    ObjectKind::Tag => {
                        let Some(target) = Tag::parse(&object.data)?.object() else {
                            continue;
                        };
                        if lost.contains(&target) {
                            found.push(format!("tag {oid}, reached from {name}, names {target}"));
                        }
                        commits.push(target);
                    }
                    ObjectKind::Commit => {
                        let commit = Commit::parse(&object.data)?;
                        for parent in commit.parents() {
                            if lost.contains(&parent) {
                                let from = format!("commit {oid}, reached from {name}");
                                found.push(format!("{from}, has the parent {parent}"));
                            }
                            commits.push(parent);
                        }
                        let mut trees: Vec<_> = commit
                            .tree()
                            .map(|tree| (tree, String::new()))
                            .into_iter()
                            .collect();
                        while let Some((tree, at)) = trees.pop() {
                            if lost.contains(&tree) {
                                let from = format!("commit {oid}, reached from {name}");
                                found.push(format!("{from}, names the tree {tree} at /{at}"));
                            }
                            if lost.contains(&tree) || !seen.insert(tree) {
                                continue;
                            }
                            let Some(object) = self.odb.get(&tree)? else {
                                continue;
                            };
                            for entry in Tree::parse(&object.data, algo)?.entries {
                                let path = format!("{at}{}", entry.name);
                                if entry.mode.is_tree() {
                                    trees.push((entry.oid, format!("{path}/")));
                                } else if lost.contains(&entry.oid) {
                                    let from = format!("commit {oid}, reached from {name}");
                                    found.push(format!("{from}, names {} at {path}", entry.oid));
                                }
                            }
                        }
                    }
                    ObjectKind::Tree | ObjectKind::Blob => {}
                }
            }
        }
        Ok(found)
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
        let path = self.alt_dir.join(COMMITTED_LOST);
        let kept = match std::fs::read(&path) {
            Ok(kept) => kept,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
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
        if lost.is_empty() {
            let _ = std::fs::remove_file(path);
        }
        Ok(lost)
    }
}
