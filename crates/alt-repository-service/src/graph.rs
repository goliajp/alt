use crate::{Error, Result};
use alt_git_codec::{Commit, HashAlgo, ObjectId, ObjectKind, Tag, Tree};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Read;

/// The adapter must expose only this repository's pinned objects and this
/// request's staged objects. Missing and unauthorized objects are indistinguishable.
/// Readers must enforce timeouts/cancellation; graph limits bound bytes and work.
pub trait ObjectSource {
    fn open(&self, oid: ObjectId) -> Result<Option<ObjectReader<'_>>>;
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
    fn edges(&self) -> Vec<(ObjectId, ObjectKind)> {
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
    nodes: BTreeMap<ObjectId, Node>,
    limits: GraphLimits,
    pub bytes: u64,
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
    pub fn objects(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.nodes.keys().copied()
    }
    pub fn reachable(&self, root: ObjectId) -> Result<Vec<ObjectId>> {
        let mut seen = BTreeSet::new();
        let mut pending = vec![root];
        let mut budget = self.budget();
        while let Some(oid) = pending.pop() {
            budget.step()?;
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

pub fn verify_graph(
    source: &impl ObjectSource,
    algo: HashAlgo,
    roots: &[ObjectId],
    limits: GraphLimits,
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
    let mut pending: VecDeque<_> = roots.iter().copied().collect();
    let mut queued: BTreeSet<_> = roots.iter().copied().collect();
    if queued.len() > limits.objects || roots.len() > limits.edges {
        return Err(Error::Limit("roots"));
    }
    let mut bytes = 0u64;
    let mut edges = 0usize;
    while let Some(oid) = pending.pop_front() {
        if nodes.contains_key(&oid) {
            continue;
        }
        check_oid(oid, algo)?;
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
    // An object reused at a different edge must still have the required type.
    for node in nodes.values() {
        for (oid, kind) in node.edges() {
            if nodes.get(&oid).map(Node::kind) != Some(kind) {
                return Err(Error::Integrity("dependency type mismatch"));
            }
        }
    }
    Ok(VerifiedGraph {
        nodes,
        limits,
        bytes,
    })
}
fn check_oid(oid: ObjectId, algo: HashAlgo) -> Result<()> {
    if oid.algo() != algo || oid.as_bytes().iter().all(|b| *b == 0) {
        return Err(Error::Integrity("object id algorithm or zero sentinel"));
    }
    Ok(())
}
fn parse_oid(bytes: &[u8], algo: HashAlgo) -> Result<ObjectId> {
    let oid = ObjectId::from_hex(bytes).map_err(|_| Error::Integrity("malformed object id"))?;
    check_oid(oid, algo)?;
    Ok(oid)
}
pub fn verify_object(
    oid: ObjectId,
    mut object: ObjectReader<'_>,
    limits: GraphLimits,
) -> Result<Node> {
    if object.bytes > limits.object_bytes {
        return Err(Error::Limit("object bytes"));
    }
    let structured = object.kind != ObjectKind::Blob;
    if structured && object.bytes > limits.structured_bytes as u64 {
        return Err(Error::Limit("structured object bytes"));
    }
    let mut data = Vec::new();
    let mut sha1 = alt_hash::Sha1::new();
    let mut sha256 = alt_hash::Sha256::new();
    let header = format!("{} {}\0", object.kind, object.bytes);
    sha1.update(header.as_bytes());
    sha256.update(header.as_bytes());
    let mut remaining = object.bytes;
    let mut buffer = [0u8; 65536];
    while remaining > 0 {
        let n = object
            .reader
            .read(&mut buffer[..remaining.min(65536) as usize])?;
        if n == 0 {
            return Err(Error::Integrity("truncated object"));
        }
        let bytes = &buffer[..n];
        match oid.algo() {
            HashAlgo::Sha1 => sha1.update(bytes),
            HashAlgo::Sha256 => sha256.update(bytes),
        }
        if structured {
            data.extend_from_slice(bytes);
        }
        remaining -= n as u64;
    }
    if object.reader.read(&mut buffer[..1])? != 0 {
        return Err(Error::Integrity("object size mismatch"));
    }
    let actual = match oid.algo() {
        HashAlgo::Sha1 => ObjectId::Sha1(sha1.finalize().into()),
        HashAlgo::Sha256 => ObjectId::Sha256(sha256.finalize().into()),
    };
    if actual != oid {
        return Err(Error::Integrity("object hash mismatch"));
    }
    parse_node(object.kind, &data, oid.algo())
}
fn single_header<'a>(
    headers: &'a [(bstr::BString, bstr::BString)],
    name: &[u8],
) -> Result<&'a [u8]> {
    let mut values = headers
        .iter()
        .filter(|(key, _)| key.as_slice() == name)
        .map(|(_, value)| value.as_slice());
    let value = values
        .next()
        .ok_or(Error::Integrity("missing required header"))?;
    if values.next().is_some() {
        return Err(Error::Integrity("duplicate required header"));
    }
    Ok(value)
}
pub fn parse_node(kind: ObjectKind, data: &[u8], algo: HashAlgo) -> Result<Node> {
    Ok(match kind {
        ObjectKind::Blob => Node::Blob,
        ObjectKind::Commit => {
            let commit = Commit::parse(data).map_err(|_| Error::Integrity("commit encoding"))?;
            let headers = commit.headers();
            let tree = parse_oid(single_header(headers, b"tree")?, algo)?;
            single_header(headers, b"author")?;
            single_header(headers, b"committer")?;
            let parents = headers
                .iter()
                .filter(|(key, _)| key.as_slice() == b"parent")
                .map(|(_, value)| parse_oid(value, algo))
                .collect::<Result<Vec<_>>>()?;
            Node::Commit { tree, parents }
        }
        ObjectKind::Tag => {
            let tag = Tag::parse(data).map_err(|_| Error::Integrity("tag encoding"))?;
            let target = parse_oid(single_header(tag.headers(), b"object")?, algo)?;
            let kind = ObjectKind::from_bytes(single_header(tag.headers(), b"type")?)
                .map_err(|_| Error::Integrity("tag target kind"))?;
            if single_header(tag.headers(), b"tag")?.is_empty() {
                return Err(Error::Integrity("empty tag name"));
            }
            Node::Tag { target, kind }
        }
        ObjectKind::Tree => {
            let tree = Tree::parse(data, algo).map_err(|_| Error::Integrity("tree encoding"))?;
            let mut names = BTreeSet::new();
            let mut entries = Vec::new();
            for entry in tree.entries {
                let name = entry.name.to_vec();
                if name.is_empty()
                    || name == b"."
                    || name == b".."
                    || name.contains(&b'/')
                    || name.eq_ignore_ascii_case(b".git")
                    || name.eq_ignore_ascii_case(b".alt")
                    || !names.insert(name.clone())
                {
                    return Err(Error::Integrity("unsafe or duplicate tree name"));
                }
                let mode = entry.mode.value();
                if !matches!(mode, 0o040000 | 0o100644 | 0o100755 | 0o120000 | 0o160000) {
                    return Err(Error::Integrity("unsupported tree mode"));
                }
                check_oid(entry.oid, algo)?;
                entries.push(TreeLink {
                    name,
                    mode,
                    oid: entry.oid,
                });
            }
            // Canonical Git tree order treats directories as if suffixed '/'.
            let sort_key = |entry: &TreeLink| {
                let mut key = entry.name.clone();
                if entry.mode == 0o040000 {
                    key.push(b'/');
                }
                key
            };
            if entries
                .windows(2)
                .any(|pair| sort_key(&pair[0]) >= sort_key(&pair[1]))
            {
                return Err(Error::Integrity("unsorted tree"));
            }
            Node::Tree(entries)
        }
    })
}
