//! Reading and verifying what roots reach: every object, or what is new.

use crate::object::{check_oid, verify_object};
use crate::{Error, GraphLimits, Node, ObjectSource, Result, VerifiedGraph};
use alt_git_codec::{HashAlgo, ObjectId};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// A tree a publication carries that no commit of it names, with the trees
/// it was made from, such as the tree of a candidate of a task and the tree of
/// the baseline of the task. Of what `git fsck` refuses, what the tree has
/// where one of those had it is not brought anew, as for the tree of a commit
/// and the trees of its parents.
#[derive(Clone, Debug)]
pub struct MadeFrom {
    pub tree: ObjectId,
    pub from: Vec<ObjectId>,
}

/// Reads and verifies every object reachable from `roots`. Of what `git fsck`
/// refuses, only what the objects the source reports as not settled bring is
/// refused: what the repository holds was taken before, as a plain
/// publication does not read it again.
pub fn verify_graph(
    source: &impl ObjectSource,
    algo: HashAlgo,
    roots: &[ObjectId],
    limits: GraphLimits,
) -> Result<VerifiedGraph> {
    verify_graph_made_from(source, algo, roots, &[], limits)
}
/// [`verify_graph`] where trees no commit names were made from others
/// ([`MadeFrom`]).
pub fn verify_graph_made_from(
    source: &impl ObjectSource,
    algo: HashAlgo,
    roots: &[ObjectId],
    made_from: &[MadeFrom],
    limits: GraphLimits,
) -> Result<VerifiedGraph> {
    verify(source, algo, (roots, made_from), limits, (false, true))
}
/// [`verify_graph`] of objects that are sent to a repository that is not their
/// source: what `git fsck` refuses is told by the repository that receives
/// them, which knows what it holds, and is not refused here.
pub fn verify_graph_to_send(
    source: &impl ObjectSource,
    algo: HashAlgo,
    roots: &[ObjectId],
    limits: GraphLimits,
) -> Result<VerifiedGraph> {
    verify(source, algo, (roots, &[]), limits, (false, false))
}
/// Reads and verifies what `roots` reach down to the objects the source reports as
/// settled, so the work follows what is new and not the history behind it.
pub fn verify_new_graph(
    source: &impl ObjectSource,
    algo: HashAlgo,
    roots: &[ObjectId],
    limits: GraphLimits,
) -> Result<VerifiedGraph> {
    verify(source, algo, (roots, &[]), limits, (true, true))
}
fn verify(
    source: &impl ObjectSource,
    algo: HashAlgo,
    (roots, made_from): (&[ObjectId], &[MadeFrom]),
    limits: GraphLimits,
    (stop_at_settled, refuse): (bool, bool),
) -> Result<VerifiedGraph> {
    if limits.objects == 0
        || limits.edges == 0
        || limits.object_bytes == 0
        || limits.structured_bytes == 0
        || limits.total_bytes == 0
        || limits.paths == 0
        || limits.path_bytes == 0
        || limits.walk_steps == 0
    {
        return Err(Error::Limit("invalid graph limits"));
    }
    let mut nodes = BTreeMap::new();
    let mut settled = BTreeMap::new();
    let mut pending: VecDeque<_> = roots.iter().copied().collect();
    let mut queued: BTreeSet<_> = roots.iter().copied().collect();
    if queued.len() > limits.objects || roots.len() > limits.edges {
        return Err(Error::Limit("roots"));
    }
    let mut bytes = 0u64;
    let mut edges = 0usize;
    while let Some(oid) = pending.pop_front() {
        if nodes.contains_key(&oid) || settled.contains_key(&oid) {
            continue;
        }
        check_oid(oid, algo)?;
        if stop_at_settled && let Some(kind) = source.settled(oid)? {
            settled.insert(oid, kind);
            continue;
        }
        let object = source.open(oid)?.ok_or(Error::Missing(oid))?;
        bytes = bytes
            .checked_add(object.bytes)
            .ok_or(Error::Limit("total bytes"))?;
        if bytes > limits.total_bytes {
            return Err(Error::Limit("total bytes"));
        }
        let node = verify_object(oid, object, limits)?;
        let dependencies = node.edges();
        edges = edges
            .checked_add(dependencies.len())
            .ok_or(Error::Limit("edges"))?;
        if edges > limits.edges {
            return Err(Error::Limit("edges"));
        }
        for (dependency, _) in dependencies {
            check_oid(dependency, algo)?;
            if queued.insert(dependency) {
                if queued.len() > limits.objects {
                    return Err(Error::Limit("objects"));
                }
                pending.push_back(dependency);
            }
        }
        nodes.insert(oid, node);
    }
    // an object reused at a different edge must still have the required type
    for node in nodes.values() {
        for (oid, kind) in node.edges() {
            let found = nodes
                .get(&oid)
                .map(Node::kind)
                .or(settled.get(&oid).copied());
            if found != Some(kind) {
                return Err(Error::Integrity("dependency type mismatch"));
            }
        }
    }
    if refuse {
        let mut brought = BTreeSet::new();
        for oid in nodes.keys() {
            if stop_at_settled || source.settled(*oid)?.is_none() {
                brought.insert(*oid);
            }
        }
        crate::fsck::refused_anew(source, (&nodes, &brought), made_from, limits)?;
    }
    Ok(VerifiedGraph {
        nodes,
        settled,
        limits,
        bytes,
    })
}
