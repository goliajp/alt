//! Where the history a fetch is given ends: the commits a shallow fetch
//! is told to hold without their parents, and those it held so and now
//! gets the parents of.
//!
//! The rules are those of `git upload-pack`. A depth counts commits from
//! the ones asked for, or with `deepen-relative` from the ones the sender
//! holds without parents; a time and a list of refs leave out what is
//! older and what the refs reach, and the commits next to what is left
//! out are the ends.

use std::rc::Rc;

use alt_git_codec::{Commit, ObjectId, ObjectKind, Tag};
use alt_hash::{KeyedIdMap, KeyedIdSet};
use alt_repo::{Ends, Repository};
use alt_wire::fetch::{Depth, FetchRequest, ShallowInfo};

type Res<T> = Result<T, Box<dyn std::error::Error>>;
type Ids = KeyedIdSet<ObjectId>;

/// What a fetch with ends to its history is answered with.
#[derive(Default)]
pub(super) struct Shallow {
    /// the lines of `shallow-info`, where the request asked for an end
    pub(super) told: Option<Vec<ShallowInfo>>,
    pub(super) ends: Ends,
    /// the parents of the commits that are held without them no longer:
    /// given as what is asked for is
    pub(super) parents: Vec<ObjectId>,
}

struct Node {
    parents: Vec<ObjectId>,
    date: i64,
}

struct History<'a> {
    repo: &'a Repository,
    nodes: KeyedIdMap<ObjectId, Option<Rc<Node>>>,
}

impl History<'_> {
    /// The commit `oid` is, and `None` for what is no commit or is not in
    /// the store.
    fn node(&mut self, oid: ObjectId) -> Res<Option<Rc<Node>>> {
        if let Some(known) = self.nodes.get(&oid) {
            return Ok(known.clone());
        }
        let node = match self.repo.read_object(&oid)? {
            Some(object) if object.kind == ObjectKind::Commit => {
                let commit = Commit::parse(&object.data)?;
                Some(Rc::new(Node {
                    parents: commit.parents().collect(),
                    date: commit.committer_date().unwrap_or(0),
                }))
            }
            _ => None,
        };
        self.nodes.insert(oid, node.clone());
        Ok(node)
    }

    /// The commit `oid` is or, through tags, names.
    fn commit(&mut self, mut oid: ObjectId) -> Res<Option<ObjectId>> {
        loop {
            match self.repo.read_object(&oid)? {
                Some(object) if object.kind == ObjectKind::Tag => {
                    match Tag::parse(&object.data)?.object() {
                        Some(named) => oid = named,
                        None => return Ok(None),
                    }
                }
                Some(object) if object.kind == ObjectKind::Commit => return Ok(Some(oid)),
                _ => return Ok(None),
            }
        }
    }

    /// The commits `tips` reach, themselves among them.
    fn reached(&mut self, tips: &[ObjectId]) -> Res<Ids> {
        let mut reached = Ids::default();
        let mut ahead = tips.to_vec();
        while let Some(oid) = ahead.pop() {
            if let Some(node) = self.node(oid)?
                && reached.insert(oid)
            {
                ahead.extend(
                    node.parents
                        .iter()
                        .filter(|parent| !reached.contains(parent)),
                );
            }
        }
        Ok(reached)
    }

    /// The commits fewer than `depth` commits down from `tips`, a tip
    /// being one down, and the commits at that depth: the ends.
    fn down(&mut self, tips: Vec<ObjectId>, depth: u32) -> Res<(Ids, Vec<ObjectId>)> {
        let (mut seen, mut inside, mut ends) = (Ids::default(), Ids::default(), Vec::new());
        let (mut level, mut at) = (tips, 1u32);
        while !level.is_empty() {
            let mut next = Vec::new();
            for oid in level {
                let Some(node) = self.node(oid)? else {
                    continue;
                };
                if !seen.insert(oid) {
                    continue;
                }
                if at >= depth {
                    ends.push(oid);
                } else {
                    inside.insert(oid);
                    next.extend(node.parents.iter().filter(|parent| !seen.contains(parent)));
                }
            }
            level = next;
            at = at.saturating_add(1);
        }
        Ok((inside, ends))
    }

    /// The commits `tips` reach that are no older than `since` and that
    /// `not` do not reach, less those that have a parent left out: the
    /// ends. A commit under one that is too old is not looked at.
    fn left(
        &mut self,
        tips: Vec<ObjectId>,
        since: Option<i64>,
        not: &[ObjectId],
    ) -> Res<(Ids, Vec<ObjectId>)> {
        let out = self.reached(not)?;
        let (mut within, mut order) = (Ids::default(), Vec::new());
        let mut ahead = tips;
        while let Some(oid) = ahead.pop() {
            if out.contains(&oid) || within.contains(&oid) {
                continue;
            }
            let Some(node) = self.node(oid)? else {
                continue;
            };
            if since.is_some_and(|since| node.date < since) {
                continue;
            }
            within.insert(oid);
            order.push(oid);
            ahead.extend(node.parents.iter().copied());
        }
        let mut ends = Vec::new();
        for oid in order {
            let node = self.node(oid)?.ok_or("a commit read is read no more")?;
            if node.parents.iter().any(|parent| !within.contains(parent)) {
                ends.push(oid);
            }
        }
        for end in &ends {
            within.remove(end);
        }
        Ok((within, ends))
    }
}

/// The one ref `name` is short for, as Git expands a name, peeled to its
/// commit. `None` where it is short for none or for more than one.
fn named(history: &mut History, name: &str) -> Res<Option<ObjectId>> {
    let full = [
        name.to_owned(),
        format!("refs/{name}"),
        format!("refs/tags/{name}"),
        format!("refs/heads/{name}"),
        format!("refs/remotes/{name}"),
        format!("refs/remotes/{name}/HEAD"),
    ];
    let refs = history.repo.list_refs()?;
    let mut found = refs.iter().filter(|(of, ..)| full.contains(of));
    match (found.next(), found.next()) {
        (Some((_, oid, _)), None) => history.commit(*oid),
        _ => Ok(None),
    }
}

/// The ends of the history `req` is given, or why it is given none: what
/// `git upload-pack` dies of.
pub(super) fn ends(repo: &Repository, req: &FetchRequest) -> Res<Result<Shallow, String>> {
    let depth: &Depth = &req.depth;
    let theirs: Ids = depth.shallows.iter().copied().collect();
    if !depth.asked() {
        let ends = Ends {
            given: theirs.clone(),
            held: theirs,
        };
        return Ok(Ok(Shallow {
            ends,
            ..Shallow::default()
        }));
    }
    let by_revs = depth.since.is_some() || !depth.not.is_empty();
    if depth.deepen.is_some() && by_revs {
        let why = "deepen and deepen-since (or deepen-not) cannot be used together";
        return Ok(Err(why.to_owned()));
    }
    let mut history = History {
        repo,
        nodes: KeyedIdMap::default(),
    };
    let mut tips = Vec::new();
    for want in &req.wants {
        tips.extend(history.commit(*want)?);
    }
    let (inside, ends) = match depth.deepen {
        Some(count) if depth.relative => {
            let reached = history.reached(&tips)?;
            let from = depth.shallows.iter().filter(|held| reached.contains(held));
            history.down(from.copied().collect(), count.saturating_add(1))?
        }
        Some(count) => history.down(tips, count)?,
        None => {
            let mut not = Vec::new();
            for name in &depth.not {
                match named(&mut history, name)? {
                    Some(oid) => not.push(oid),
                    None => return Ok(Err(format!("ambiguous deepen-not: {name}"))),
                }
            }
            let left = history.left(tips, depth.since, &not)?;
            if left.0.is_empty() && left.1.is_empty() {
                return Ok(Err("no commits selected for shallow requests".to_owned()));
            }
            left
        }
    };
    let mut given: Ids = ends.iter().copied().collect();
    let mut told = Vec::new();
    told.extend(
        ends.iter()
            .filter(|end| !theirs.contains(end))
            .map(|end| ShallowInfo::Shallow(*end)),
    );
    let mut parents = Vec::new();
    let mut looked = Ids::default();
    for held in depth.shallows.iter().filter(|held| looked.insert(**held)) {
        match history.node(*held)? {
            Some(node) if inside.contains(held) => {
                told.push(ShallowInfo::Unshallow(*held));
                parents.extend(node.parents.iter().copied());
            }
            _ => drop(given.insert(*held)),
        }
    }
    Ok(Ok(Shallow {
        told: Some(told),
        ends: Ends {
            given,
            held: theirs,
        },
        parents,
    }))
}
