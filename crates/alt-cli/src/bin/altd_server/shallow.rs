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

use super::fetch::Until;

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
    /// how long the fetch is gone on with: looked at before a commit is read
    until: &'a Until<'a>,
}

impl History<'_> {
    /// The commit `oid` is, and `None` for what is no commit or is not in
    /// the store.
    fn node(&mut self, oid: ObjectId) -> Res<Option<Rc<Node>>> {
        if let Some(known) = self.nodes.get(&oid) {
            return Ok(known.clone());
        }
        self.until.go_on()?;
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
            self.until.go_on()?;
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
fn named(history: &mut History, refs: &Refs, name: &str) -> Res<Option<ObjectId>> {
    let full = [
        name.to_owned(),
        format!("refs/{name}"),
        format!("refs/tags/{name}"),
        format!("refs/heads/{name}"),
        format!("refs/remotes/{name}"),
        format!("refs/remotes/{name}/HEAD"),
    ];
    let mut found = refs.iter().filter(|(of, ..)| full.contains(of));
    match (found.next(), found.next()) {
        (Some((_, oid, _)), None) => history.commit(*oid),
        _ => Ok(None),
    }
}

/// The ends of the history `req` is given, or why it is given none: what
/// `git upload-pack` dies of.
pub(super) fn ends(
    repo: &Repository,
    req: &FetchRequest,
    until: &Until,
) -> Res<Result<Shallow, String>> {
    ends_of(repo, req, until, &mut || Ok(repo.list_refs()?))
}

/// The refs of a repository, as [`Repository::list_refs`] gives them.
type Refs = Vec<(String, ObjectId, Option<String>)>;

/// [`ends`], with the refs read through `listed`: once at the most for a
/// request, however many of them it names.
fn ends_of(
    repo: &Repository,
    req: &FetchRequest,
    until: &Until,
    listed: &mut dyn FnMut() -> Res<Refs>,
) -> Res<Result<Shallow, String>> {
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
        until,
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
            // read once for all the names a request gives, and not for none
            let refs = depth.not.first().map_or(Ok(Vec::new()), |_| listed())?;
            for name in &depth.not {
                match named(&mut history, &refs, name)? {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A request may name 1024 refs whose histories it leaves out. The refs of
    /// the repository are read once for all of them, and the answer is that
    /// of the request with each name once.
    #[test]
    fn the_refs_are_read_once_for_all_the_names_a_request_leaves_out() {
        let dir = alt_os::temp::tempdir().unwrap();
        alt_testutil::make_repo(dir.path(), "sha1");
        let repo = Repository::discover(dir.path()).unwrap();
        let tip = |name: &str| {
            let refs = repo.list_refs().unwrap();
            let found = refs.into_iter().find(|(of, ..)| of == name);
            found.unwrap().1
        };
        let asked = |not: Vec<String>| {
            let depth = Depth {
                not,
                ..Depth::default()
            };
            let req = FetchRequest {
                wants: vec![tip("refs/heads/main")],
                depth,
                ..FetchRequest::default()
            };
            let mut read = 0;
            let ends = ends_of(&repo, &req, &Until::whenever(), &mut || {
                read += 1;
                Ok(repo.list_refs()?)
            });
            let told = ends.unwrap().unwrap().told.unwrap();
            (read, format!("{told:?}"))
        };
        // the branch that was merged, as three of the names it has
        let names = ["feat", "refs/heads/feat", "heads/feat"];
        let many: Vec<String> = (0..1024).map(|n| names[n % 3].to_owned()).collect();
        let (read, told) = asked(many);
        assert_eq!(read, 1);
        let (once, same) = asked(names.map(str::to_owned).to_vec());
        assert_eq!((once, &same), (1, &told));
        assert!(told.contains("Shallow"), "{told}");
        // no name: the refs are not read at all, and the request asks for no end
        let none = FetchRequest {
            wants: vec![tip("refs/heads/main")],
            ..FetchRequest::default()
        };
        let mut read = 0;
        let ends = ends_of(&repo, &none, &Until::whenever(), &mut || {
            read += 1;
            Ok(repo.list_refs()?)
        });
        assert!(ends.unwrap().unwrap().told.is_none());
        assert_eq!(read, 0);
    }

    /// A request for an end to its history names what it wants and what it
    /// leaves out, a million ids and a thousand names at the most: each is
    /// read to find the commit it is. A fetch whose time is over reads none
    /// of them, though none is a commit whose history would be gone down.
    #[test]
    fn what_a_request_names_is_read_for_as_long_as_the_fetch_is_gone_on_with() {
        let dir = alt_os::temp::tempdir().unwrap();
        alt_testutil::make_repo(dir.path(), "sha1");
        let repo = Repository::discover(dir.path()).unwrap();
        // ids of nothing in the store: no history is gone down for them
        let wants: Vec<ObjectId> = (1..=4u8)
            .map(|n| ObjectId::from_bytes(repo.algo().unwrap(), &[n; 20]).unwrap())
            .collect();
        let depth = Depth {
            deepen: Some(1),
            ..Depth::default()
        };
        let req = FetchRequest {
            wants,
            depth,
            ..FetchRequest::default()
        };
        let asked = |until: &Until| ends_of(&repo, &req, until, &mut || Ok(repo.list_refs()?));
        assert!(asked(&Until::whenever()).is_ok());
        let over = Until {
            by: Some(std::time::Instant::now()),
            gone: None,
        };
        let stopped = asked(&over).map(drop).unwrap_err();
        let late = super::super::until::Stopped::Late;
        assert_eq!(stopped.downcast_ref(), Some(&late), "{stopped}");
    }
}
