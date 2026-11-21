//! The trees of the commits a push adds.

use alt_git_codec::{ObjectId, ObjectKind};

use super::tests::{Held, id};

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
/// with `dirs` directories, each of them new. The parents differ in a
/// file and have the directories alike.
fn reads_of_a_merge(parents: usize, dirs: usize) -> usize {
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
    let merge = held.commit_of(merged, &served, 1_000_000);
    held.reads.set(0);
    let added = held.added(&[merge], &served, &[merge]).unwrap();
    // the merge, its root and file, and each directory with its file
    assert_eq!(added.len(), 3 + 2 * dirs);
    held.reads.get()
}

/// Each tree is read once, those of the push and those the refs reach
/// in their place, and each commit twice at most.
#[test]
fn the_trees_of_a_merge_of_many_parents_are_each_read_once() {
    for (parents, dirs) in [(200, 200), (400, 400)] {
        let reads = reads_of_a_merge(parents, dirs);
        let trees = (1 + dirs) + (parents + dirs);
        let most = trees + 2 * (parents + 1);
        assert!(
            reads <= most,
            "{reads} reads for {trees} trees of {parents} parents"
        );
    }
}
