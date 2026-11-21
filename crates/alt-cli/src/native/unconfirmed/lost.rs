//! What names an object that goes when records naming content which is
//! not stored are taken out of the object store.

use std::collections::HashSet;

use alt_git_codec::{Commit, ObjectKind, Tag, Tree};

use super::*;

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

    /// Takes away, for every workspace, the note that the blobs its index
    /// stages were all found stored: objects have gone since.
    pub(crate) fn forget_staged_seen(&self) {
        let _ = std::fs::remove_file(self.alt_dir.join("index.seen"));
        if let Ok(listing) = std::fs::read_dir(self.alt_dir.join("workspaces")) {
            for entry in listing.filter_map(|entry| entry.ok()) {
                let _ = std::fs::remove_file(entry.path().join("index.seen"));
            }
        }
    }
}
