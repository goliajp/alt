//! The commits a push adds to the history a repository serves: those a
//! new value of the push reaches and no ref reached before it.
//!
//! Two marks are passed down from commit to parent, one from the new
//! values and one from the refs as they are. Commits are visited newest
//! committer date first, so that the mark of the refs usually gets to a
//! commit before the commit is visited. The dates decide that order and
//! nothing else: a commit that gets a mark after it was visited is
//! visited again and passes the mark on.
//!
//! A commit visited with the mark of the push alone is asked whether it
//! fails the policy, and one that does stays a suspect until the mark of
//! the refs reaches it. Every commit the push adds is visited that way:
//! the commits between it and a new value are added too, so none of them
//! ever has the other mark. The walk ends
//!
//! - when no commit with the mark of the push alone waits to be visited
//!   and no suspect is left: the push adds nothing that fails, or
//! - when nothing waits to be visited: every commit under the refs has
//!   their mark by then, so a suspect left is a commit the push adds.
//!
//! A commit is read when it gets its first mark and queued once for each
//! mark, which bounds the walk by the commits reachable, whatever their
//! dates.

use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};

use alt_git_codec::{Commit, ObjectId, ObjectKind, RawObject, Tag};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Where the walk reads from.
pub struct Source<'a> {
    /// an object, `None` when it is not held
    pub get: &'a dyn Fn(&ObjectId) -> Res<Option<RawObject>>,
    /// why a commit fails the policy, when it does
    pub fails: &'a dyn Fn(&ObjectId) -> Res<Option<String>>,
}

const ADDED: u8 = 1;
const SERVED: u8 = 2;

struct Node {
    marks: u8,
    /// the marks its parents have from it
    passed: u8,
    parents: Vec<ObjectId>,
    date: i64,
    /// set for a commit that does not read as one
    unreadable: Option<String>,
}

struct Walk<'a> {
    source: &'a Source<'a>,
    /// `None` for what is no commit the source holds
    nodes: HashMap<ObjectId, Option<Node>>,
    queue: BinaryHeap<(i64, ObjectId)>,
    /// how many commits wait to be visited with the mark of the push alone
    open: usize,
    /// visited commits that fail the policy and the refs are not known
    /// to reach
    suspects: BTreeMap<ObjectId, String>,
}

impl Walk<'_> {
    fn load(&self, oid: &ObjectId) -> Res<Option<Node>> {
        let Some(raw) = (self.source.get)(oid)? else {
            return Ok(None);
        };
        if raw.kind != ObjectKind::Commit {
            return Ok(None);
        }
        let (parents, date, unreadable) = match Commit::parse(&raw.data) {
            Ok(commit) => (
                commit.parents().collect(),
                commit.committer_date().unwrap_or(0),
                None,
            ),
            Err(e) => (Vec::new(), 0, Some(format!("commit {oid} unreadable: {e}"))),
        };
        Ok(Some(Node {
            marks: 0,
            passed: 0,
            parents,
            date,
            unreadable,
        }))
    }

    /// Gives the commit `oid` the mark. Whether `oid` is a commit.
    fn mark(&mut self, oid: ObjectId, mark: u8) -> Res<bool> {
        if !self.nodes.contains_key(&oid) {
            let node = self.load(&oid)?;
            self.nodes.insert(oid, node);
        }
        let Some(node) = self.nodes.get_mut(&oid).and_then(Option::as_mut) else {
            return Ok(false);
        };
        if node.marks & mark != 0 {
            return Ok(true);
        }
        let waited = node.marks == ADDED && node.passed == 0;
        node.marks |= mark;
        self.queue.push((node.date, oid));
        if node.marks == ADDED {
            self.open += 1;
        } else if waited {
            self.open -= 1;
        } else if mark == SERVED {
            self.suspects.remove(&oid);
        }
        Ok(true)
    }

    /// The reason one of the commits the push adds fails with.
    fn refusal(mut self) -> Res<Option<String>> {
        while self.open > 0 || !self.suspects.is_empty() {
            let Some((_, oid)) = self.queue.pop() else {
                break;
            };
            let Some(node) = self.nodes.get_mut(&oid).and_then(Option::as_mut) else {
                continue;
            };
            let new = node.marks & !node.passed;
            if new == 0 {
                continue;
            }
            let only_added = node.marks == ADDED;
            node.passed = node.marks;
            let parents = node.parents.clone();
            if only_added {
                self.open -= 1;
                let reason = match node.unreadable.clone() {
                    Some(reason) => Some(reason),
                    None => (self.source.fails)(&oid)?,
                };
                if let Some(reason) = reason {
                    self.suspects.insert(oid, reason);
                }
            }
            for parent in parents {
                for mark in [ADDED, SERVED] {
                    if new & mark != 0 {
                        self.mark(parent, mark)?;
                    }
                }
            }
        }
        Ok(self.suspects.into_values().next())
    }
}

/// What a tag names, `Err` with the reason for a tag that does not read
/// as one; `None` for anything else.
fn tagged(source: &Source<'_>, oid: &ObjectId) -> Res<Option<Result<ObjectId, String>>> {
    Ok(match (source.get)(oid)? {
        Some(raw) if raw.kind == ObjectKind::Tag => match Tag::parse(&raw.data) {
            Ok(tag) => tag.object().map(Ok),
            Err(e) => Some(Err(format!("tag {oid} unreadable: {e}"))),
        },
        _ => None,
    })
}

/// The reason to refuse a push that points refs at `new`, where the refs
/// point at `served`: a commit it adds fails the policy, or what it adds
/// cannot be told because a new value is at a tag that does not read as
/// one. An object under the refs that does not read is passed over: it
/// was there before the push.
pub fn refusal(source: &Source<'_>, served: &[ObjectId], new: &[ObjectId]) -> Res<Option<String>> {
    let mut walk = Walk {
        source,
        nodes: HashMap::new(),
        queue: BinaryHeap::new(),
        open: 0,
        suspects: BTreeMap::new(),
    };
    // what the refs name, through any tags
    let mut named = HashSet::new();
    for &tip in served {
        let mut oid = tip;
        while named.insert(oid) && !walk.mark(oid, SERVED)? {
            match tagged(source, &oid)? {
                Some(Ok(target)) => oid = target,
                _ => break,
            }
        }
    }
    for &tip in new {
        let mut oid = tip;
        while !named.contains(&oid) && !walk.mark(oid, ADDED)? {
            match tagged(source, &oid)? {
                Some(Ok(target)) => oid = target,
                Some(Err(reason)) => return Ok(Some(reason)),
                None => break,
            }
        }
    }
    walk.refusal()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use alt_git_codec::HashAlgo;

    use super::*;

    #[derive(Default)]
    struct Held {
        objects: HashMap<ObjectId, RawObject>,
        reads: Cell<usize>,
    }

    impl Held {
        fn put(&mut self, kind: ObjectKind, data: &[u8]) -> ObjectId {
            let oid = ObjectId::hash_object(HashAlgo::Sha1, kind, data);
            let data = data.to_vec();
            self.objects.insert(oid, RawObject { kind, data });
            oid
        }

        fn commit(&mut self, parents: &[ObjectId], date: u64, message: &str) -> ObjectId {
            let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, b"");
            let mut text = format!("tree {tree}\n");
            for parent in parents {
                text.push_str(&format!("parent {parent}\n"));
            }
            let who = format!("tester <t@e> {date} +0000");
            text.push_str(&format!("author {who}\ncommitter {who}\n\n{message}\n"));
            self.put(ObjectKind::Commit, text.as_bytes())
        }

        fn tag(&mut self, target: &ObjectId) -> ObjectId {
            let text =
                format!("object {target}\ntype commit\ntag t\ntagger tester <t@e> 1 +0000\n\nt\n");
            self.put(ObjectKind::Tag, text.as_bytes())
        }

        /// The refusal where every commit but those of `passing` fails.
        fn refusal(
            &self,
            passing: &[ObjectId],
            served: &[ObjectId],
            new: &[ObjectId],
        ) -> Option<String> {
            let read = |oid: &ObjectId| {
                self.reads.set(self.reads.get() + 1);
                self.objects.get(oid)
            };
            let get = |oid: &ObjectId| -> Res<Option<RawObject>> {
                Ok(read(oid).map(|raw| RawObject {
                    kind: raw.kind,
                    data: raw.data.clone(),
                }))
            };
            let fails = |oid: &ObjectId| -> Res<Option<String>> {
                read(oid);
                Ok((!passing.contains(oid)).then(|| format!("{oid} fails")))
            };
            let source = Source {
                get: &get,
                fails: &fails,
            };
            super::refusal(&source, served, new).unwrap()
        }
    }

    /// A chain of `len` commits whose dates follow no order, the last one
    /// dated before all the others.
    fn chain(held: &mut Held, len: u64) -> Vec<ObjectId> {
        let mut word = 0x9e37_79b9_7f4a_7c15_u64;
        let mut commits: Vec<ObjectId> = Vec::new();
        for n in 0..len {
            word ^= word << 13;
            word ^= word >> 7;
            word ^= word << 17;
            let date = match n + 1 == len {
                true => 1,
                false => 1_000_000 + word % 1_000_000,
            };
            let parents: Vec<ObjectId> = commits.last().copied().into_iter().collect();
            commits.push(held.commit(&parents, date, &format!("commit {n}")));
        }
        commits
    }

    /// The reads of a push that goes through the ancestors of a served
    /// commit dated before them, all failing, and not through the commit.
    fn reads_past_an_early_tip(len: u64) -> usize {
        let mut held = Held::default();
        let commits = chain(&mut held, len);
        let tip = commits[commits.len() - 1];
        let under = commits[commits.len() - 2];
        let new = held.commit(&[under], 3_000_000, "new");
        assert_eq!(held.refusal(&[new], &[tip], &[new]), None);
        held.reads.get()
    }

    #[test]
    fn served_commits_behind_an_early_dated_tip_are_each_read_a_fixed_number_of_times() {
        let (short, long) = (reads_past_an_early_tip(4000), reads_past_an_early_tip(8000));
        assert!(short <= 2 * 4000 + 8, "{short} reads for 4000 commits");
        assert!(long <= 2 * 8000 + 8, "{long} reads for 8000 commits");
    }

    #[test]
    fn a_failing_commit_only_the_push_reaches_refuses_whatever_the_dates() {
        let mut held = Held::default();
        let commits = chain(&mut held, 200);
        let tip = commits[commits.len() - 1];
        // older than everything served, and under a commit that passes
        let failing = held.commit(&[commits[10]], 0, "failing");
        let new = held.commit(&[failing], 3_000_000, "new");
        let refusal = held.refusal(&[new], &[tip], &[new]);
        assert_eq!(refusal, Some(format!("{failing} fails")));
    }

    #[test]
    fn a_push_on_top_of_dated_history_reads_none_of_it() {
        let mut held = Held::default();
        let mut tip = held.commit(&[], 1, "root");
        for date in 2..1000 {
            tip = held.commit(&[tip], date, "next");
        }
        let new = held.commit(&[tip], 2000, "new");
        assert_eq!(held.refusal(&[new], &[tip], &[new]), None);
        assert!(held.reads.get() <= 4, "{} reads", held.reads.get());
    }

    #[test]
    fn what_does_not_read_is_passed_over_under_the_refs_and_refused_as_a_new_value() {
        let mut held = Held::default();
        let root = held.commit(&[], 1, "root");
        let bad_tag = held.put(ObjectKind::Tag, b"not a tag");
        let bad_commit = held.put(ObjectKind::Commit, b"not a commit");
        let new = held.commit(&[root], 2, "new");
        let served = [root, bad_tag, bad_commit];
        assert_eq!(held.refusal(&[new], &served, &[new]), None);
        // the refs name them already: nothing is added
        assert_eq!(held.refusal(&[], &served, &[bad_tag, bad_commit]), None);
        let tag = held.tag(&root);
        assert_eq!(held.refusal(&[], &[tag], &[tag, root]), None);

        let refusal = held.refusal(&[], &[root], &[bad_tag]).unwrap();
        assert!(refusal.contains("unreadable"), "{refusal}");
        let refusal = held.refusal(&[], &[root], &[bad_commit]).unwrap();
        assert!(refusal.contains("unreadable"), "{refusal}");
        let failing = held.commit(&[root], 3, "failing");
        let tag = held.tag(&failing);
        assert_eq!(
            held.refusal(&[], &[root], &[tag]),
            Some(format!("{failing} fails"))
        );
    }
}
