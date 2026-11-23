//! Which commits a log shows: those a range leaves, and of them those
//! that change the paths asked about.
//!
//! A range is worked out as git works it out. The commits are taken
//! newest first, those of the side that is left out among them; one of
//! that side marks its parents as left out, down through every commit
//! read so far. The walk ends when nothing of the other side waits and
//! what waits is older than the last commit kept, with five more commits
//! taken for good measure: the dates commits carry are not always in the
//! order of the history.
//!
//! Paths cut the history down as git's default does: a commit that has
//! the paths as one of its parents has them is not shown, and only that
//! parent is followed.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use alt_git_codec::{Commit, ObjectId, ObjectKind, Tree};
use alt_repo::Repository;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// A commit as a walk needs it: when it was made, its parents and tree.
#[derive(Clone)]
struct Node {
    date: i64,
    parents: Vec<ObjectId>,
    tree: Option<ObjectId>,
    data: Vec<u8>,
}

/// The commits a walk has read, each once.
struct Graph<'r> {
    repo: &'r Repository,
    nodes: HashMap<ObjectId, Node>,
}

impl<'r> Graph<'r> {
    fn new(repo: &'r Repository) -> Self {
        Graph {
            repo,
            nodes: HashMap::new(),
        }
    }

    fn node(&mut self, oid: ObjectId) -> Res<&Node> {
        if !self.nodes.contains_key(&oid) {
            let (commit, object) = self.repo.peel(oid)?;
            if object.kind != ObjectKind::Commit {
                return Err(format!("{commit} is not a commit").into());
            }
            let parsed = Commit::parse(&object.data)?;
            let node = Node {
                date: parsed.committer_date().unwrap_or(0),
                parents: parsed.parents().collect(),
                tree: parsed.tree(),
                data: object.data,
            };
            self.nodes.insert(oid, node);
        }
        Ok(&self.nodes[&oid])
    }
}

/// How many commits a walk goes on for once it seems to be done.
const SLOP: u32 = 5;

/// The commits reachable from `include` and not from `exclude`.
fn limited(graph: &mut Graph, include: ObjectId, exclude: ObjectId) -> Res<HashSet<ObjectId>> {
    let mut queue: BinaryHeap<(i64, Reverse<u64>, ObjectId)> = BinaryHeap::new();
    let (mut seen, mut left_out) = (HashSet::new(), HashSet::new());
    let mut order = 0u64;
    left_out.insert(exclude);
    for start in [include, exclude] {
        if seen.insert(start) {
            order += 1;
            queue.push((graph.node(start)?.date, Reverse(order), start));
        }
    }
    let (mut kept, mut last, mut slop) = (Vec::new(), i64::MAX, SLOP);
    while let Some((date, _, oid)) = queue.pop() {
        let parents = graph.node(oid)?.parents.clone();
        if !left_out.contains(&oid) {
            for parent in parents {
                if seen.insert(parent) {
                    order += 1;
                    queue.push((graph.node(parent)?.date, Reverse(order), parent));
                }
            }
            (last, slop) = (date, SLOP);
            kept.push(oid);
            continue;
        }
        // left out with every commit read so far that it leads to
        let mut below = parents.clone();
        while let Some(commit) = below.pop() {
            if left_out.insert(commit)
                && let Some(read) = graph.nodes.get(&commit)
            {
                below.extend(read.parents.iter().copied());
            }
        }
        for parent in parents {
            if seen.insert(parent) {
                order += 1;
                queue.push((graph.node(parent)?.date, Reverse(order), parent));
            }
        }
        let waiting = queue.iter().any(|(_, _, oid)| !left_out.contains(oid));
        slop = match queue.peek() {
            None => 0,
            Some((next, ..)) if last <= *next || waiting => SLOP,
            Some(_) => slop - 1,
        };
        if slop == 0 {
            break;
        }
    }
    kept.retain(|oid| !left_out.contains(oid));
    Ok(kept.into_iter().collect())
}

/// What a log is asked to show.
pub(crate) struct Selection {
    /// the commits it starts from
    starts: Vec<ObjectId>,
    /// the commits a range leaves; every commit where there is no range
    within: Option<HashSet<ObjectId>>,
}

/// The two sides of `A..B` or `A...B` and whether it has three dots; an
/// empty side is `HEAD`.
pub(crate) fn sides(spec: &str) -> Option<(&str, &str, bool)> {
    let (left, right, both) = match spec.split_once("...") {
        Some((left, right)) => (left, right, true),
        None => spec.split_once("..").map(|(l, r)| (l, r, false))?,
    };
    let or_head = |side| if str::is_empty(side) { "HEAD" } else { side };
    let (left, right) = (or_head(left), or_head(right));
    Some((left, right, both))
}

impl Selection {
    /// What `spec` selects: one revision and all that leads to it, `A..B`
    /// or `A...B`. `named` is how the revision was given, for an error.
    pub(crate) fn of(repo: &Repository, spec: &str, named: &str) -> Res<Selection> {
        let resolve = |rev: &str| -> Res<ObjectId> {
            let found = repo.rev_parse(rev)?;
            let oid = found.ok_or_else(|| {
                crate::git_settings::fatal(format!(
                    "ambiguous argument '{named}': unknown revision or path not in the working tree."
                ))
            })?;
            Ok(repo.peel(oid)?.0)
        };
        let Some((left, right, both)) = sides(spec) else {
            return Ok(Selection {
                starts: vec![resolve(spec)?],
                within: None,
            });
        };
        let (left, right) = (resolve(left)?, resolve(right)?);
        let mut graph = Graph::new(repo);
        let mut within = limited(&mut graph, right, left)?;
        let mut starts = vec![right];
        if both {
            within.extend(limited(&mut graph, left, right)?);
            starts.insert(0, left);
        }
        Ok(Selection {
            starts,
            within: Some(within),
        })
    }

    /// The commits selected, newest first, each with the bytes of its
    /// object; of those only the ones that change one of `paths`, which
    /// are from the root of the tree, where there are any. At most
    /// `limit` of them.
    pub(crate) fn commits(
        &self,
        repo: &Repository,
        paths: &[String],
        limit: usize,
    ) -> Res<Vec<(ObjectId, Vec<u8>)>> {
        let mut graph = Graph::new(repo);
        let allowed = |oid: &ObjectId| self.within.as_ref().is_none_or(|set| set.contains(oid));
        let mut queue: BinaryHeap<(i64, Reverse<u64>, ObjectId)> = BinaryHeap::new();
        let (mut seen, mut order) = (HashSet::new(), 0u64);
        for start in self.starts.iter().filter(|oid| allowed(oid)) {
            if seen.insert(*start) {
                order += 1;
                queue.push((graph.node(*start)?.date, Reverse(order), *start));
            }
        }
        let mut shown = Vec::new();
        while shown.len() < limit
            && let Some((_, _, oid)) = queue.pop()
        {
            let node = graph.node(oid)?.clone();
            let (show, follow) = match paths.is_empty() {
                true => (true, node.parents.clone()),
                false => changes(&mut graph, &node, paths)?,
            };
            if show {
                shown.push((oid, node.data));
            }
            for parent in follow.into_iter().filter(|parent| allowed(parent)) {
                if seen.insert(parent) {
                    order += 1;
                    queue.push((graph.node(parent)?.date, Reverse(order), parent));
                }
            }
        }
        Ok(shown)
    }
}

/// Whether the commit `node` is shown in a log of `paths`, and the
/// parents the log goes on with.
fn changes(graph: &mut Graph, node: &Node, paths: &[String]) -> Res<(bool, Vec<ObjectId>)> {
    let mine = at_paths(graph.repo, node.tree, paths)?;
    if node.parents.is_empty() {
        return Ok((mine.iter().any(Option::is_some), Vec::new()));
    }
    for parent in &node.parents {
        let tree = graph.node(*parent)?.tree;
        if at_paths(graph.repo, tree, paths)? == mine {
            return Ok((false, vec![*parent]));
        }
    }
    Ok((true, node.parents.clone()))
}

/// What `tree` has at each of `paths`: the mode and id of the file or
/// the directory there.
fn at_paths(
    repo: &Repository,
    tree: Option<ObjectId>,
    paths: &[String],
) -> Res<Vec<Option<(u32, ObjectId)>>> {
    let Some(tree) = tree else {
        return Ok(vec![None; paths.len()]);
    };
    paths
        .iter()
        .map(|path| entry_at(repo, tree, path))
        .collect()
}

/// The entry at `path` in `tree`, the tree itself for the empty path.
fn entry_at(repo: &Repository, tree: ObjectId, path: &str) -> Res<Option<(u32, ObjectId)>> {
    let mut found = (0o040000, tree);
    for part in path.split('/').filter(|part| !part.is_empty()) {
        if found.0 != 0o040000 {
            return Ok(None);
        }
        let object = repo.read_object(&found.1)?;
        let object = object.ok_or_else(|| format!("tree {} missing", found.1))?;
        let Some(entry) = Tree::find(&object.data, repo.algo()?, part.as_bytes())? else {
            return Ok(None);
        };
        found = entry;
    }
    Ok(Some(found))
}
