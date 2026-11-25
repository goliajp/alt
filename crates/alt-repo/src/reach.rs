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
        let reached = self.walk(roots, excludes, ends, true, &|| true)?;
        Ok(reached.into_iter().map(|at| (at.oid, at.kind)).collect())
    }

    /// [`Self::reachable_objects_between`] with the name each object was
    /// come to under, and with no file read: a file is told by the entry
    /// of the tree it is in, so one the store lacks is listed all the same.
    pub fn reachable_named_between(
        &self,
        roots: &[ObjectId],
        excludes: &[ObjectId],
        ends: &Ends,
    ) -> Result<Vec<Reached>, RepoError> {
        self.walk(roots, excludes, ends, false, &|| true)
    }

    /// [`Self::reachable_named_between`] for as long as `go_on` says, which
    /// is asked before each object is read: [`RepoError::Stopped`] once it
    /// says no.
    pub fn reachable_named_while(
        &self,
        roots: &[ObjectId],
        excludes: &[ObjectId],
        ends: &Ends,
        go_on: &dyn Fn() -> bool,
    ) -> Result<Vec<Reached>, RepoError> {
        self.walk(roots, excludes, ends, false, go_on)
    }

    fn walk(
        &self,
        roots: &[ObjectId],
        excludes: &[ObjectId],
        ends: &Ends,
        files_read: bool,
        go_on: &dyn Fn() -> bool,
    ) -> Result<Vec<Reached>, RepoError> {
        let unnamed = |oid: &ObjectId| Under {
            oid: *oid,
            name: 0,
            file: false,
        };
        let mut uninteresting = KeyedIdSet::<ObjectId>::default();
        let mut stack: Vec<Under> = excludes.iter().map(unnamed).collect();
        while let Some(at) = stack.pop() {
            if !uninteresting.insert(at.oid) || (at.file && !files_read) {
                continue;
            }
            if !go_on() {
                return Err(RepoError::Stopped);
            }
            if let Some(obj) = self.read_object(&at.oid)? {
                stack.extend(children(&obj, at.oid, &ends.held)?);
            }
        }

        let mut seen = KeyedIdSet::<ObjectId>::default();
        let mut out: Vec<Reached> = Vec::new();
        let mut work: Vec<Under> = roots
            .iter()
            .filter(|o| !uninteresting.contains(o))
            .map(unnamed)
            .collect();
        while let Some(at) = work.pop() {
            if uninteresting.contains(&at.oid) || !seen.insert(at.oid) {
                continue;
            }
            let (oid, name) = (at.oid, at.name);
            if at.file && !files_read {
                let kind = ObjectKind::Blob;
                out.push(Reached { oid, kind, name });
                continue;
            }
            if !go_on() {
                return Err(RepoError::Stopped);
            }
            let Some(obj) = self.read_object(&oid)? else {
                // a root pointing at a missing object is the caller's bug
                // (push wants must be present locally); skip rather than
                // crash so the caller can surface a precise error
                continue;
            };
            let kind = obj.kind;
            out.push(Reached { oid, kind, name });
            for child in children(&obj, oid, &ends.given)? {
                if !uninteresting.contains(&child.oid) {
                    work.push(child);
                }
            }
        }
        Ok(out)
    }
}

/// An object [`Repository::reachable_named_between`] came to. `name` is a
/// number made of the end of the name it has in the tree it was found in,
/// 0 for what no tree names: the versions of one file have one number, and
/// so do most files of one name or one ending.
pub struct Reached {
    pub oid: ObjectId,
    pub kind: ObjectKind,
    pub name: u32,
}

struct Under {
    oid: ObjectId,
    name: u32,
    file: bool,
}

/// The number Git sorts the objects of a pack by before it looks for
/// deltas among them: the last sixteen bytes of the name weigh most.
fn named(name: &[u8]) -> u32 {
    name.iter()
        .filter(|byte| !byte.is_ascii_whitespace())
        .fold(0u32, |hash, byte| {
            (hash >> 2).wrapping_add(u32::from(*byte) << 24)
        })
}

/// What `obj` points at. A commit in `ends` has its tree and no parents.
fn children(
    obj: &RawObject,
    oid: ObjectId,
    ends: &KeyedIdSet<ObjectId>,
) -> Result<Vec<Under>, RepoError> {
    let unnamed = |oid: ObjectId| Under {
        oid,
        name: 0,
        file: false,
    };
    let mut out = Vec::new();
    match obj.kind {
        ObjectKind::Commit => {
            let commit = Commit::parse(&obj.data)?;
            out.extend(commit.tree().map(unnamed));
            if !ends.contains(&oid) {
                out.extend(commit.parents().map(unnamed));
            }
        }
        ObjectKind::Tree => {
            let algo = oid.algo();
            for entry in Tree::raw_entries(&obj.data, algo) {
                let (mode, name, oid) = entry?;
                // a gitlink (mode 160000) points at a commit of another
                // repository, which is not in this store
                if !mode.is_gitlink() {
                    out.push(Under {
                        oid: ObjectId::from_bytes(algo, oid).expect("as long as an id"),
                        name: named(name),
                        file: mode.object_kind() == ObjectKind::Blob,
                    });
                }
            }
        }
        ObjectKind::Tag => out.extend(Tag::parse(&obj.data)?.object().map(unnamed)),
        ObjectKind::Blob => {}
    }
    Ok(out)
}
