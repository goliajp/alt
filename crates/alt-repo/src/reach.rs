//! The objects a set of tips reaches and another set does not: what a
//! push sends and what a fetch is given.

use alt_git_codec::{Commit, ObjectId, ObjectKind, RawObject, Tag, Tree};
use alt_hash::KeyedIdSet;

use crate::{RepoError, Repository};

/// Where the two sides of a shallow fetch stop following history: the
/// commits whose parents are not given, and the commits whose parents the
/// other side does not hold.
#[derive(Default)]
pub struct Ends {
    pub given: KeyedIdSet<ObjectId>,
    pub held: KeyedIdSet<ObjectId>,
}

impl Repository {
    /// Enumerate every object (commit, tree, blob, tag) reachable from
    /// `roots`, excluding everything reachable from `excludes`. The result
    /// is the set of objects that need to ship in a push: `roots` are the
    /// tips the client wants to update to, `excludes` are tips the server
    /// already has (so the closure under `excludes` is what the server can
    /// reconstruct deltas against).
    ///
    /// The returned vector lists each object exactly once with its kind so
    /// a downstream pack writer can stream them without re-reading. Order
    /// is unspecified — `PackWriter` handles plain entries in any order.
    pub fn reachable_objects(
        &self,
        roots: &[ObjectId],
        excludes: &[ObjectId],
    ) -> Result<Vec<(ObjectId, ObjectKind)>, RepoError> {
        self.reachable_objects_between(roots, excludes, &Ends::default())
    }

    /// [`Self::reachable_objects`] of a history that has ends: the parents
    /// of a commit in `ends.given` are not followed from `roots`, and
    /// those of a commit in `ends.held` are not followed from `excludes`.
    pub fn reachable_objects_between(
        &self,
        roots: &[ObjectId],
        excludes: &[ObjectId],
        ends: &Ends,
    ) -> Result<Vec<(ObjectId, ObjectKind)>, RepoError> {
        let mut uninteresting = KeyedIdSet::<ObjectId>::default();
        let mut stack: Vec<ObjectId> = excludes.to_vec();
        while let Some(oid) = stack.pop() {
            if !uninteresting.insert(oid) {
                continue;
            }
            if let Some(obj) = self.read_object(&oid)? {
                stack.extend(children(&obj, oid, &ends.held)?);
            }
        }

        let mut seen = KeyedIdSet::<ObjectId>::default();
        let mut out: Vec<(ObjectId, ObjectKind)> = Vec::new();
        let mut work: Vec<ObjectId> = roots
            .iter()
            .filter(|o| !uninteresting.contains(o))
            .copied()
            .collect();
        while let Some(oid) = work.pop() {
            if uninteresting.contains(&oid) || !seen.insert(oid) {
                continue;
            }
            let Some(obj) = self.read_object(&oid)? else {
                // a root pointing at a missing object is the caller's bug
                // (push wants must be present locally); skip rather than
                // crash so the caller can surface a precise error
                continue;
            };
            out.push((oid, obj.kind));
            for child in children(&obj, oid, &ends.given)? {
                if !uninteresting.contains(&child) {
                    work.push(child);
                }
            }
        }
        Ok(out)
    }
}

/// What `obj` points at. A commit in `ends` has its tree and no parents.
fn children(
    obj: &RawObject,
    oid: ObjectId,
    ends: &KeyedIdSet<ObjectId>,
) -> Result<Vec<ObjectId>, RepoError> {
    let mut out = Vec::new();
    match obj.kind {
        ObjectKind::Commit => {
            let commit = Commit::parse(&obj.data)?;
            out.extend(commit.tree());
            if !ends.contains(&oid) {
                out.extend(commit.parents());
            }
        }
        ObjectKind::Tree => {
            for entry in &Tree::parse(&obj.data, oid.algo())?.entries {
                // a gitlink (mode 160000) points at a commit of another
                // repository, which is not in this store
                if !entry.mode.is_gitlink() {
                    out.push(entry.oid);
                }
            }
        }
        ObjectKind::Tag => out.extend(Tag::parse(&obj.data)?.object()),
        ObjectKind::Blob => {}
    }
    Ok(out)
}
