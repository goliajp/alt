//! The trees of the commits a push adds.

use alt_git_codec::{ObjectId, ObjectKind};

use super::MISSING;
use super::tests::{Held, id};

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

/// A gitlink names a commit of another repository: nothing asks for it.
#[test]
fn a_tree_with_a_gitlink_is_whole_without_the_commit_it_names() {
    let mut held = Held::default();
    let base = held.commit(&[], 1, "base");
    let elsewhere = id(ObjectKind::Commit, b"of another repository");
    let file = held.put(ObjectKind::Blob, b"file");
    let mut data = b"100644 file\0".to_vec();
    data.extend_from_slice(file.as_bytes());
    data.extend_from_slice(b"160000 sub\0");
    data.extend_from_slice(elsewhere.as_bytes());
    let tree = held.put(ObjectKind::Tree, &data);
    let new = held.commit_of(tree, &[base], 2);
    let added = held.added(&[new], &[base], &[new]).unwrap();
    assert!(added.contains(&tree) && added.contains(&file));
    assert!(!added.contains(&elsewhere));
}

/// The reads of a push that adds a merge of `parents` served commits
/// with `dirs` directories, each of them new, and `chain` commits on top
/// of it that each change a file. The parents differ in a file and have
/// the directories alike.
fn reads_of_a_merge(parents: usize, dirs: usize, chain: usize) -> usize {
    let mut held = Held::default();
    let names: Vec<String> = (0..dirs).map(|n| format!("d{n:04}")).collect();
    let level = |held: &mut Held, what: &str| -> Vec<ObjectId> {
        let dir = |name: &String| {
            let file = held.put(ObjectKind::Blob, format!("{what} {name}").as_bytes());
            held.tree(&[("f", false, file)])
        };
        names.iter().map(dir).collect()
    };
    let (old, new) = (level(&mut held, "old"), level(&mut held, "new"));
    let root = |held: &mut Held, dirs: &[ObjectId], id: &str| {
        let id = held.put(ObjectKind::Blob, id.as_bytes());
        let dirs = names
            .iter()
            .zip(dirs)
            .map(|(name, oid)| (&name[..], true, *oid));
        let entries: Vec<_> = dirs.chain([("id", false, id)]).collect();
        held.tree(&entries)
    };
    let served: Vec<ObjectId> = (0..parents)
        .map(|n| {
            let root = root(&mut held, &old, &format!("parent {n}"));
            held.commit_of(root, &[], 1 + n as u64)
        })
        .collect();
    let merged = root(&mut held, &new, "merge");
    let mut pushed = vec![held.commit_of(merged, &served, 1_000_000)];
    for n in 0..chain {
        let root = root(&mut held, &new, &format!("chain {n}"));
        pushed.push(held.commit_of(root, &pushed[n..], 1_000_001 + n as u64));
    }
    held.reads.set(0);
    let added = held.added(&pushed, &served, &pushed[chain..]).unwrap();
    // each commit with its root and file, and each directory with its file
    assert_eq!(added.len(), 3 * (1 + chain) + 2 * dirs);
    held.reads.get()
}

/// Each tree is read once, those of the push and those the refs reach
/// in their place, and each commit twice at most: whatever the number
/// of parents a tree of the push is held against, and of commits of the
/// push that are held against the same parents.
#[test]
fn the_trees_of_a_merge_of_many_parents_are_each_read_once() {
    for (parents, dirs, chain) in [(200, 200, 0), (200, 0, 200), (400, 400, 400)] {
        let reads = reads_of_a_merge(parents, dirs, chain);
        let trees = (1 + chain + dirs) + (parents + dirs);
        let most = trees + 2 * (parents + 1 + chain);
        assert!(
            reads <= most,
            "{reads} reads for {trees} trees of {parents} parents"
        );
    }
}
