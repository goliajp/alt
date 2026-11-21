use std::cell::Cell;
use std::collections::HashMap;

use alt_git_codec::HashAlgo;

use super::*;

#[derive(Default)]
pub(super) struct Held {
    objects: HashMap<ObjectId, RawObject>,
    /// what only the pack of the push holds
    packed: HashSet<ObjectId>,
    pub(super) reads: Cell<usize>,
}

pub(super) fn id(kind: ObjectKind, data: &[u8]) -> ObjectId {
    ObjectId::hash_object(HashAlgo::Sha1, kind, data)
}

impl Held {
    pub(super) fn put(&mut self, kind: ObjectKind, data: &[u8]) -> ObjectId {
        let oid = id(kind, data);
        let data = data.to_vec();
        self.objects.insert(oid, RawObject { kind, data });
        oid
    }

    /// A tree of `entries`: name, whether a directory, object.
    pub(super) fn tree(&mut self, entries: &[(&str, bool, ObjectId)]) -> ObjectId {
        let mut data = Vec::new();
        for (name, dir, oid) in entries {
            let mode = if *dir { "40000" } else { "100644" };
            data.extend_from_slice(format!("{mode} {name}\0").as_bytes());
            data.extend_from_slice(oid.as_bytes());
        }
        self.put(ObjectKind::Tree, &data)
    }

    pub(super) fn commit_of(
        &mut self,
        tree: ObjectId,
        parents: &[ObjectId],
        date: u64,
    ) -> ObjectId {
        let mut text = format!("tree {tree}\n");
        for parent in parents {
            text.push_str(&format!("parent {parent}\n"));
        }
        let who = format!("tester <t@e> {date} +0000");
        text.push_str(&format!("author {who}\ncommitter {who}\n\nat {date}\n"));
        self.put(ObjectKind::Commit, text.as_bytes())
    }

    /// A commit of the empty tree.
    pub(super) fn commit(&mut self, parents: &[ObjectId], date: u64, message: &str) -> ObjectId {
        let tree = self.put(ObjectKind::Tree, b"");
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

    /// What the push adds where every commit but those of `passing`
    /// fails the policy.
    pub(super) fn added(
        &self,
        passing: &[ObjectId],
        served: &[ObjectId],
        new: &[ObjectId],
    ) -> Result<HashSet<ObjectId>, String> {
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
        let holds = |oid: &ObjectId| -> Res<bool> { Ok(self.objects.contains_key(oid)) };
        let had = |oid: &ObjectId| -> Res<bool> {
            Ok(self.objects.contains_key(oid) && !self.packed.contains(oid))
        };
        let fails = |oid: &ObjectId| -> Res<Option<String>> {
            read(oid);
            Ok((!passing.contains(oid)).then(|| format!("{oid} fails")))
        };
        let source = Source {
            get: &get,
            holds: &holds,
            had: &had,
            fails: &fails,
        };
        super::added(&source, served, new).unwrap()
    }

    pub(super) fn refusal(
        &self,
        passing: &[ObjectId],
        served: &[ObjectId],
        new: &[ObjectId],
    ) -> Option<String> {
        self.added(passing, served, new).err()
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
    held.packed.insert(new);
    assert_eq!(held.added(&[new], &[tip], &[new]), Ok(HashSet::from([new])));
    assert!(held.reads.get() <= 4, "{} reads", held.reads.get());
}

/// What the refs point at is read only once the walk comes to a commit
/// held before the push that none of them points at.
#[test]
fn a_push_on_top_of_one_ref_reads_nothing_the_other_refs_point_at() {
    let mut held = Held::default();
    let root = held.commit(&[], 1, "root");
    let mut served = vec![root];
    for n in 0..10_000 {
        served.push(held.commit(&[root], 2, &format!("branch {n}")));
    }
    let tip = served[5000];
    let new = held.commit(&[tip], 3, "new");
    held.packed.insert(new);
    assert_eq!(held.refusal(&[new], &served, &[new]), None);
    assert!(held.reads.get() <= 4, "{} reads", held.reads.get());

    // a branch from under a ref: each of them is read, once
    held.reads.set(0);
    let side = held.commit(&[root], 4, "side");
    held.packed.insert(side);
    let detached: Vec<ObjectId> = served[1..].to_vec();
    assert_eq!(held.refusal(&[side], &detached, &[side]), None);
    let reads = held.reads.get();
    assert!((10_000..=10_008).contains(&reads), "{reads} reads");
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

#[test]
fn a_new_value_and_everything_it_adds_has_to_be_held() {
    let mut held = Held::default();
    let root = held.commit(&[], 1, "root");
    let absent = id(ObjectKind::Commit, b"absent");
    let missing = Some(MISSING.to_owned());
    assert_eq!(held.refusal(&[], &[root], &[absent]), missing);

    // a commit whose parent is not held, at any depth
    let orphan = held.commit(&[absent], 2, "orphan");
    let above = held.commit(&[orphan], 3, "above");
    assert_eq!(held.refusal(&[orphan, above], &[root], &[above]), missing);
    assert_eq!(held.refusal(&[orphan], &[root], &[orphan]), missing);
    // a tag at a commit that is not held, and one above such a commit
    let tag = held.tag(&absent);
    assert_eq!(held.refusal(&[], &[root], &[tag]), missing);
    let tag = held.tag(&orphan);
    assert_eq!(held.refusal(&[orphan], &[root], &[tag]), missing);
}

#[test]
fn the_tree_of_an_added_commit_has_to_be_whole() {
    let mut held = Held::default();
    let blob = held.put(ObjectKind::Blob, b"held");
    let absent_blob = id(ObjectKind::Blob, b"absent");
    let absent_tree = id(ObjectKind::Tree, b"absent");
    let missing = Some(MISSING.to_owned());

    let whole = held.tree(&[("a", false, blob)]);
    let dir = held.tree(&[("dir", true, whole)]);
    let root = held.commit_of(dir, &[], 1);
    assert_eq!(held.refusal(&[root], &[], &[root]), None);

    // no tree at all, a blob three levels down, a tree one level down
    let cases = [
        absent_tree,
        {
            let low = held.tree(&[("a", false, blob), ("b", false, absent_blob)]);
            let mid = held.tree(&[("low", true, low)]);
            held.tree(&[("dir", true, whole), ("mid", true, mid)])
        },
        held.tree(&[("dir", true, absent_tree)]),
    ];
    for (n, tree) in cases.into_iter().enumerate() {
        let commit = held.commit_of(tree, &[root], 2 + n as u64);
        assert_eq!(held.refusal(&[commit], &[root], &[commit]), missing, "{n}");
        // a new value that is the tree itself
        assert_eq!(held.refusal(&[], &[root], &[tree]), missing, "{n}");
    }
}

/// A change taken back: the newest and the oldest commit have one tree,
/// the commit between them another, and a blob is in both. Each commit
/// is looked at before its parent.
#[test]
fn what_two_added_commits_share_is_added_once_whatever_the_order() {
    let mut held = Held::default();
    let kept = held.put(ObjectKind::Blob, b"kept");
    let (one, other) = (
        held.put(ObjectKind::Blob, b"one"),
        held.put(ObjectKind::Blob, b"other"),
    );
    let first = held.tree(&[("kept", false, kept), ("varies", false, one)]);
    let second = held.tree(&[("kept", false, kept), ("varies", false, other)]);
    let roots = [first, second].map(|dir| held.tree(&[("dir", true, dir)]));
    let oldest = held.commit_of(roots[0], &[], 1);
    let between = held.commit_of(roots[1], &[oldest], 2);
    let newest = held.commit_of(roots[0], &[between], 3);
    let all = [oldest, between, newest];
    let added = held.added(&all, &[], &[newest]).unwrap();
    let mut expected = HashSet::from(all);
    expected.extend([kept, one, other, first, second]);
    expected.extend(roots);
    assert_eq!(added, expected);
}

/// A tree is read where it differs from what the refs reach, and what
/// is the same as there is not asked for, in the commit on top of that
/// history and in the next one.
#[test]
fn a_tree_is_looked_at_where_it_differs_from_what_the_refs_reach() {
    let mut held = Held::default();
    let blob = held.put(ObjectKind::Blob, b"held");
    let mut dirs = Vec::new();
    for n in 0..200 {
        let name = format!("file {n}");
        dirs.push(held.tree(&[(name.as_str(), false, blob)]));
    }
    let names: Vec<String> = (0..200).map(|n| format!("dir {n:03}")).collect();
    fn entries<'a>(names: &'a [String], dirs: &[ObjectId]) -> Vec<(&'a str, bool, ObjectId)> {
        let named = names.iter().zip(dirs);
        named
            .map(|(name, dir)| (name.as_str(), true, *dir))
            .collect()
    }
    let before = held.tree(&entries(&names, &dirs));
    let served = held.commit_of(before, &[], 1);
    let changed = held.put(ObjectKind::Blob, b"changed");
    dirs[77] = held.tree(&[("file 77", false, changed)]);
    let after = held.tree(&entries(&names, &dirs));
    let new = held.commit_of(after, &[served], 2);
    for oid in [new, after, dirs[77], changed] {
        held.packed.insert(oid);
    }
    let added = held.added(&[new], &[served], &[new]).unwrap();
    assert_eq!(added, HashSet::from([new, after, dirs[77], changed]));
    // the two commits, the policy, the two roots, the two directories
    assert!(held.reads.get() <= 8, "{} reads", held.reads.get());

    let again = held.put(ObjectKind::Blob, b"changed again");
    dirs[78] = held.tree(&[("file 78", false, again)]);
    let last = held.tree(&entries(&names, &dirs));
    let next = held.commit_of(last, &[new], 3);
    for oid in [next, last, dirs[78], again] {
        held.packed.insert(oid);
    }
    held.reads.set(0);
    let added = held.added(&[new, next], &[served], &[next]).unwrap();
    let both = [new, after, dirs[77], changed, next, last, dirs[78], again];
    assert_eq!(added, HashSet::from(both));
    // three commits, the policy twice, three roots twice (the served
    // one once for each commit), four directories
    assert!(held.reads.get() <= 15, "{} reads", held.reads.get());
}

/// History under the refs that lacks a commit stays as it is: a push on
/// top of it is let in, and the commit, sent along, is not among what
/// the push adds.
#[test]
fn an_object_missing_under_the_refs_is_passed_over_and_not_added() {
    let mut held = Held::default();
    let lost = held.commit(&[], 1, "lost");
    let served = held.commit(&[lost], 2, "served");
    let new = held.commit(&[served], 3, "new");
    held.packed.extend([lost, new]);
    assert_eq!(
        held.added(&[new], &[served], &[new]),
        Ok(HashSet::from([new]))
    );
    // with the commit not sent
    held.objects.remove(&lost);
    assert_eq!(held.refusal(&[new], &[served], &[new]), None);
    // a new value that reaches it from elsewhere adds it
    let lost = held.commit(&[], 1, "lost");
    let beside = held.commit(&[lost], 4, "beside");
    held.packed.insert(beside);
    assert_eq!(
        held.refusal(&[beside], &[served], &[beside]),
        Some(format!("{lost} fails"))
    );
}
