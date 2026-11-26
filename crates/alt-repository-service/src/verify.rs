//! Reading and verifying what roots reach: every object, or what is new.

use crate::object::{check_oid, verify_object};
use crate::{Error, GraphLimits, Node, ObjectSource, Result, VerifiedGraph};
use alt_git_codec::{HashAlgo, ObjectId};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Reads and verifies every object reachable from `roots`.
pub fn verify_graph(
    source: &impl ObjectSource,
    algo: HashAlgo,
    roots: &[ObjectId],
    limits: GraphLimits,
) -> Result<VerifiedGraph> {
    verify(source, algo, roots, limits, false)
}
/// Reads and verifies what `roots` reach down to the objects the source reports as
/// settled, so the work follows what is new and not the history behind it.
pub fn verify_new_graph(
    source: &impl ObjectSource,
    algo: HashAlgo,
    roots: &[ObjectId],
    limits: GraphLimits,
) -> Result<VerifiedGraph> {
    verify(source, algo, roots, limits, true)
}
fn verify(
    source: &impl ObjectSource,
    algo: HashAlgo,
    roots: &[ObjectId],
    limits: GraphLimits,
    stop_at_settled: bool,
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
    crate::fsck::refused_anew(source, &nodes, limits)?;
    Ok(VerifiedGraph {
        nodes,
        settled,
        limits,
        bytes,
    })
}
