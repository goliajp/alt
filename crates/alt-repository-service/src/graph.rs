use crate::{Error, Result};
use alt_git_codec::{ObjectId, ObjectKind};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

/// The adapter must expose only this repository's pinned objects and this
/// request's staged objects. Missing and unauthorized objects are indistinguishable.
/// Readers must enforce timeouts/cancellation; graph limits bound bytes and work.
pub trait ObjectSource {
    fn open(&self, oid: ObjectId) -> Result<Option<ObjectReader<'_>>>;
    /// The kind of an object the repository already holds verified, together with
    /// everything it references. Such an object is not read again.
    fn settled(&self, _oid: ObjectId) -> Result<Option<ObjectKind>> {
        Ok(None)
    }
}
pub struct ObjectReader<'a> {
    pub kind: ObjectKind,
    pub bytes: u64,
    pub reader: Box<dyn Read + 'a>,
}
#[derive(Clone, Copy, Debug)]
pub struct GraphLimits {
    pub objects: usize,
    pub edges: usize,
    pub object_bytes: u64,
    pub total_bytes: u64,
    /// Bounds any parsed commit, tree or tag; blobs are streamed.
    pub structured_bytes: usize,
    pub paths: usize,
    pub path_bytes: usize,
    /// Aggregate ancestry/path work for one publication validation.
    pub walk_steps: usize,
}
impl From<alt_remote_protocol::Limits> for GraphLimits {
    fn from(limits: alt_remote_protocol::Limits) -> Self {
        Self {
            objects: limits.graph_objects,
            edges: limits.graph_edges,
            object_bytes: limits.object_bytes,
            total_bytes: limits.total_bytes,
            structured_bytes: limits.structured_bytes,
            paths: limits.tree_paths,
            path_bytes: limits.path_bytes,
            walk_steps: limits.walk_steps,
        }
    }
}
#[derive(Clone, Debug)]
pub struct TreeLink {
    pub name: Vec<u8>,
    pub mode: u32,
    pub oid: ObjectId,
}
#[derive(Clone, Debug)]
pub enum Node {
    Blob,
    Tree(Vec<TreeLink>),
    Commit {
        tree: ObjectId,
        parents: Vec<ObjectId>,
    },
    Tag {
        target: ObjectId,
        kind: ObjectKind,
    },
}
impl Node {
    pub fn kind(&self) -> ObjectKind {
        match self {
            Self::Blob => ObjectKind::Blob,
            Self::Tree(_) => ObjectKind::Tree,
            Self::Commit { .. } => ObjectKind::Commit,
            Self::Tag { .. } => ObjectKind::Tag,
        }
    }
    pub(crate) fn edges(&self) -> Vec<(ObjectId, ObjectKind)> {
        match self {
            Self::Blob => vec![],
            Self::Tree(entries) => entries
                .iter()
                .filter(|e| e.mode != 0o160000)
                .map(|e| {
                    (
                        e.oid,
                        if e.mode == 0o040000 {
                            ObjectKind::Tree
                        } else {
                            ObjectKind::Blob
                        },
                    )
                })
                .collect(),
            Self::Commit { tree, parents } => std::iter::once((*tree, ObjectKind::Tree))
                .chain(parents.iter().map(|p| (*p, ObjectKind::Commit)))
                .collect(),
            Self::Tag { target, kind } => vec![(*target, *kind)],
        }
    }
}
/// Read access to already-verified objects for ancestry and path checks. An
/// implementation may hold every node in memory or load each on demand.
pub trait Graph {
    fn node(&self, oid: ObjectId) -> Result<std::borrow::Cow<'_, Node>>;
    fn graph_limits(&self) -> GraphLimits;
}
impl Graph for VerifiedGraph {
    fn node(&self, oid: ObjectId) -> Result<std::borrow::Cow<'_, Node>> {
        VerifiedGraph::node(self, oid).map(std::borrow::Cow::Borrowed)
    }
    fn graph_limits(&self) -> GraphLimits {
        self.limits
    }
}
pub(crate) fn budget(graph: &(impl Graph + ?Sized)) -> WalkBudget {
    WalkBudget(graph.graph_limits().walk_steps)
}
pub(crate) fn is_ancestor_bounded(
    graph: &(impl Graph + ?Sized),
    old: ObjectId,
    new: ObjectId,
    budget: &mut WalkBudget,
) -> Result<bool> {
    let mut pending = vec![new];
    let mut seen = BTreeSet::new();
    while let Some(oid) = pending.pop() {
        budget.step()?;
        if !seen.insert(oid) {
            continue;
        }
        let node = graph.node(oid)?;
        let Node::Commit { parents, .. } = node.as_ref() else {
            return Err(Error::Integrity("ancestry target is not a commit"));
        };
        if oid == old {
            return Ok(true);
        }
        pending.extend(parents);
    }
    Ok(false)
}
pub(crate) fn changed_paths_bounded(
    graph: &(impl Graph + ?Sized),
    before: Option<ObjectId>,
    after: Option<ObjectId>,
    budget: &mut WalkBudget,
) -> Result<Vec<Vec<u8>>> {
    let before = paths(graph, before, budget)?;
    let after = paths(graph, after, budget)?;
    let names: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    Ok(names
        .into_iter()
        .filter(|p| before.get(*p) != after.get(*p))
        .cloned()
        .collect())
}
fn paths(
    graph: &(impl Graph + ?Sized),
    commit: Option<ObjectId>,
    budget: &mut WalkBudget,
) -> Result<BTreeMap<Vec<u8>, (u32, ObjectId)>> {
    let limits = graph.graph_limits();
    let mut paths = BTreeMap::new();
    let Some(commit) = commit else {
        return Ok(paths);
    };
    let node = graph.node(commit)?;
    let Node::Commit { tree, .. } = node.as_ref() else {
        return Err(Error::Forbidden("path scope requires commit targets"));
    };
    let mut pending = vec![(Vec::new(), *tree)];
    let mut work = 0usize;
    while let Some((prefix, oid)) = pending.pop() {
        budget.step()?;
        work = work.checked_add(1).ok_or(Error::Limit("tree expansion"))?;
        if work > limits.paths {
            return Err(Error::Limit("tree expansion"));
        }
        let node = graph.node(oid)?;
        let Node::Tree(entries) = node.as_ref() else {
            return Err(Error::Integrity("tree type"));
        };
        for entry in entries {
            budget.step()?;
            let mut path = prefix.clone();
            path.extend_from_slice(&entry.name);
            if path.len() > limits.path_bytes {
                return Err(Error::Limit("path bytes"));
            }
            if entry.mode == 0o040000 {
                let child = graph.node(entry.oid)?;
                let Node::Tree(children) = child.as_ref() else {
                    return Err(Error::Integrity("tree type"));
                };
                if !children.is_empty() {
                    path.push(b'/');
                    pending.push((path, entry.oid));
                    if pending.len() > limits.paths {
                        return Err(Error::Limit("tree expansion"));
                    }
                    continue;
                }
            }
            paths.insert(path, (entry.mode, entry.oid));
            if paths.len() > limits.paths {
                return Err(Error::Limit("tree paths"));
            }
        }
    }
    Ok(paths)
}
/// Only constructible by complete byte/hash/type validation. This is evidence
/// about a pinned snapshot, not a capability to publish without a transaction.
pub struct VerifiedGraph {
    pub(crate) nodes: BTreeMap<ObjectId, Node>,
    /// objects the walk stopped at because the repository already holds them verified
    pub(crate) settled: BTreeMap<ObjectId, ObjectKind>,
    pub(crate) limits: GraphLimits,
    pub bytes: u64,
}
/// A graph verified down to what the repository already holds, continued there:
/// ancestry and path checks walk on into `settled` where verification stopped.
pub struct Layered<'a, G: Graph> {
    pub verified: &'a VerifiedGraph,
    pub settled: &'a G,
}
impl<G: Graph> Graph for Layered<'_, G> {
    fn node(&self, oid: ObjectId) -> Result<std::borrow::Cow<'_, Node>> {
        match self.verified.nodes.get(&oid) {
            Some(node) => Ok(std::borrow::Cow::Borrowed(node)),
            // a settled object, or one below it: there everything is settled
            None => self.settled.node(oid),
        }
    }
    fn graph_limits(&self) -> GraphLimits {
        self.verified.limits
    }
}
pub(crate) struct WalkBudget(usize);
impl WalkBudget {
    pub(crate) fn step(&mut self) -> Result<()> {
        self.0 = self
            .0
            .checked_sub(1)
            .ok_or(Error::Limit("graph traversal work"))?;
        Ok(())
    }
}
impl VerifiedGraph {
    pub(crate) fn budget(&self) -> WalkBudget {
        WalkBudget(self.limits.walk_steps)
    }
    pub fn node(&self, oid: ObjectId) -> Result<&Node> {
        self.nodes.get(&oid).ok_or(Error::Missing(oid))
    }
    /// the objects this verification read: not those it found settled
    pub fn objects(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.nodes.keys().copied()
    }
    /// whether the object was verified here or found settled
    pub fn holds(&self, oid: ObjectId) -> bool {
        self.nodes.contains_key(&oid) || self.settled.contains_key(&oid)
    }
    /// the objects this verification read that `root` reaches
    pub fn reachable(&self, root: ObjectId) -> Result<Vec<ObjectId>> {
        let mut seen = BTreeSet::new();
        let mut pending = vec![root];
        let mut budget = self.budget();
        while let Some(oid) = pending.pop() {
            budget.step()?;
            if self.settled.contains_key(&oid) {
                continue;
            }
            if seen.insert(oid) {
                pending.extend(self.node(oid)?.edges().into_iter().map(|(id, _)| id));
            }
        }
        Ok(seen.into_iter().collect())
    }
    pub fn is_ancestor(&self, old: ObjectId, new: ObjectId) -> Result<bool> {
        self.is_ancestor_bounded(old, new, &mut self.budget())
    }
    pub(crate) fn is_ancestor_bounded(
        &self,
        old: ObjectId,
        new: ObjectId,
        budget: &mut WalkBudget,
    ) -> Result<bool> {
        is_ancestor_bounded(self, old, new, budget)
    }
    /// Changes include modes, symlink targets, gitlinks, empty trees, and raw
    /// non-UTF8 paths. A bounded iterative walk prevents recursive stack growth.
    pub fn changed_paths(
        &self,
        before: Option<ObjectId>,
        after: Option<ObjectId>,
    ) -> Result<Vec<Vec<u8>>> {
        self.changed_paths_bounded(before, after, &mut self.budget())
    }
    pub(crate) fn changed_paths_bounded(
        &self,
        before: Option<ObjectId>,
        after: Option<ObjectId>,
        budget: &mut WalkBudget,
    ) -> Result<Vec<Vec<u8>>> {
        changed_paths_bounded(self, before, after, budget)
    }
}
