//! The walk over the commits a push reaches (see the module above).

use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};

use alt_git_codec::{Commit, ObjectId, ObjectKind};

use super::trees::Trees;
use super::{MISSING, Res, Source, tagged};

pub(super) const ADDED: u8 = 1;
const SERVED: u8 = 2;

struct Node {
    marks: u8,
    /// the marks its parents have from it
    passed: u8,
    parents: Vec<ObjectId>,
    tree: Option<ObjectId>,
    date: i64,
    /// set for a commit that does not read as one
    unreadable: Option<String>,
}

pub(super) struct Walk<'a> {
    source: &'a Source<'a>,
    /// `None` for what is no commit the source holds
    nodes: HashMap<ObjectId, Option<Node>>,
    queue: BinaryHeap<(i64, ObjectId)>,
    /// how many commits wait to be visited with the mark of the push alone
    open: usize,
    /// visited commits that fail and the refs are not known to reach
    suspects: BTreeMap<ObjectId, String>,
    /// what the refs point at
    tips: HashSet<ObjectId>,
    /// whether all of `tips` have their mark
    spread: bool,
    /// the commits visited with the mark of the push alone, and how many
    /// of them had their trees looked at
    visited: Vec<ObjectId>,
    looked: usize,
    pub(super) trees: Trees,
    /// see [`Self::bases`]
    bases: HashMap<ObjectId, Vec<ObjectId>>,
}

impl<'a> Walk<'a> {
    pub(super) fn new(source: &'a Source<'a>, served: &[ObjectId]) -> Self {
        Walk {
            source,
            nodes: HashMap::new(),
            queue: BinaryHeap::new(),
            open: 0,
            suspects: BTreeMap::new(),
            tips: served.iter().copied().collect(),
            spread: false,
            visited: Vec::new(),
            looked: 0,
            bases: HashMap::new(),
            trees: Trees::holding(super::trees::BASES),
        }
    }

    /// Whether a ref points at `oid`.
    pub(super) fn tip(&self, oid: &ObjectId) -> bool {
        self.tips.contains(oid)
    }

    /// Once nothing refuses the push: the commits it adds and what is
    /// in their trees.
    pub(super) fn added_objects(self) -> impl Iterator<Item = ObjectId> {
        let nodes = self.nodes;
        let added = move |oid: &ObjectId| {
            let node = nodes.get(oid).and_then(Option::as_ref);
            node.is_some_and(|node| node.marks == ADDED)
        };
        let commits = self.visited.into_iter().filter(added);
        commits.chain(self.trees.added)
    }

    fn load(&self, oid: &ObjectId) -> Res<Option<Node>> {
        let Some(raw) = (self.source.get)(oid)? else {
            return Ok(None);
        };
        if raw.kind != ObjectKind::Commit {
            return Ok(None);
        }
        let parsed = Commit::parse(&raw.data).map_err(|e| e.to_string());
        let parsed = parsed.and_then(|commit| {
            let tree = commit.tree().ok_or("no tree")?;
            let parents = commit.parents().collect();
            Ok((parents, Some(tree), commit.committer_date().unwrap_or(0)))
        });
        let (parents, tree, date, unreadable) = match parsed {
            Ok((parents, tree, date)) => (parents, tree, date, None),
            Err(e) => (
                Vec::new(),
                None,
                0,
                Some(format!("commit {oid} unreadable: {e}")),
            ),
        };
        Ok(Some(Node {
            marks: 0,
            passed: 0,
            parents,
            tree,
            date,
            unreadable,
        }))
    }

    /// Gives the commit `oid` the mark. Whether `oid` is a commit.
    pub(super) fn mark(&mut self, oid: ObjectId, mark: u8) -> Res<bool> {
        let mut mark = mark;
        if !self.nodes.contains_key(&oid) {
            let node = self.load(&oid)?;
            self.nodes.insert(oid, node);
            if self.tips.contains(&oid) {
                mark |= SERVED;
            }
        }
        let Some(node) = self.nodes.get_mut(&oid).and_then(Option::as_mut) else {
            return Ok(false);
        };
        for one in [ADDED, SERVED] {
            if mark & one == 0 || node.marks & one != 0 {
                continue;
            }
            let waited = node.marks == ADDED && node.passed == 0;
            node.marks |= one;
            self.queue.push((node.date, oid));
            if node.marks == ADDED {
                self.open += 1;
            } else if waited {
                self.open -= 1;
            } else if one == SERVED {
                self.suspects.remove(&oid);
            }
        }
        Ok(true)
    }

    /// Gives what the refs point at, through any tags, the mark of the
    /// refs.
    fn spread(&mut self) -> Res<()> {
        self.spread = true;
        let mut named = HashSet::new();
        for tip in self.tips.clone() {
            let mut oid = tip;
            while named.insert(oid) && !self.mark(oid, SERVED)? {
                match tagged(self.source, &oid)? {
                    Some(Ok(target)) => oid = target,
                    _ => break,
                }
            }
        }
        Ok(())
    }

    /// Visits commits until none the push adds is left to visit and no
    /// suspect is left, or nothing is left to visit.
    fn settle(&mut self) -> Res<()> {
        while self.open > 0 || !self.suspects.is_empty() {
            let Some((date, oid)) = self.queue.pop() else {
                break;
            };
            let Some(node) = self.nodes.get(&oid).and_then(Option::as_ref) else {
                continue;
            };
            let new = node.marks & !node.passed;
            if new == 0 {
                continue;
            }
            let only_added = node.marks == ADDED;
            // a commit held before the push may be one the refs reach
            if only_added && !self.spread && (self.source.had)(&oid)? {
                self.queue.push((date, oid));
                self.spread()?;
                continue;
            }
            let (parents, unreadable) = (node.parents.clone(), node.unreadable.clone());
            if let Some(node) = self.nodes.get_mut(&oid).and_then(Option::as_mut) {
                node.passed = node.marks;
            }
            let mut lacks = false;
            for parent in parents {
                for mark in [ADDED, SERVED] {
                    if new & mark != 0 {
                        lacks |= !self.mark(parent, mark)?;
                    }
                }
            }
            if only_added {
                self.open -= 1;
                self.visited.push(oid);
                let reason = match unreadable {
                    Some(reason) => Some(reason),
                    None if lacks => Some(MISSING.to_owned()),
                    None => (self.source.fails)(&oid)?,
                };
                if let Some(reason) = reason {
                    self.suspects.insert(oid, reason);
                }
            }
        }
        Ok(())
    }

    /// The commit `oid` when only the push has marked it.
    fn added(&self, oid: &ObjectId) -> Option<&Node> {
        let node = self.nodes.get(oid)?.as_ref()?;
        (node.marks == ADDED).then_some(node)
    }

    /// The trees of the nearest commits under the added commit `oid`
    /// that the refs reach: what such a tree has is not looked at again
    /// (see [`Trees`]). Kept for each added commit on the way there.
    fn bases(&mut self, oid: ObjectId) -> Vec<ObjectId> {
        let mut stack = vec![oid];
        while let Some(&top) = stack.last() {
            let parents = match self.added(&top) {
                Some(commit) if !self.bases.contains_key(&top) => commit.parents.clone(),
                _ => {
                    stack.pop();
                    continue;
                }
            };
            let waits = |parent: &&ObjectId| {
                self.added(parent).is_some() && !self.bases.contains_key(*parent)
            };
            let waiting: Vec<ObjectId> = parents.iter().filter(waits).copied().collect();
            if !waiting.is_empty() {
                stack.extend(waiting);
                continue;
            }
            let mut bases = Vec::new();
            for parent in &parents {
                match self.bases.get(parent) {
                    Some(theirs) => bases.extend(theirs),
                    None => bases.extend(self.nodes.get(parent).and_then(|n| n.as_ref()?.tree)),
                }
            }
            bases.sort_unstable();
            bases.dedup();
            self.bases.insert(top, bases);
            stack.pop();
        }
        self.bases.get(&oid).cloned().unwrap_or_default()
    }

    /// The reason one of the commits the push adds fails with.
    pub(super) fn refusal(&mut self) -> Res<Option<String>> {
        loop {
            self.settle()?;
            if let Some(reason) = self.suspects.values().next() {
                return Ok(Some(reason.clone()));
            }
            while self.looked < self.visited.len() {
                let oid = self.visited[self.looked];
                self.looked += 1;
                let Some(tree) = self.added(&oid).and_then(|commit| commit.tree) else {
                    continue;
                };
                let bases = self.bases(oid);
                if !self.trees.whole(self.source, tree, &bases)? {
                    self.suspects.insert(oid, MISSING.to_owned());
                }
            }
            if self.suspects.is_empty() {
                return Ok(None);
            }
        }
    }
}
