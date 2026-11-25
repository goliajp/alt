use super::kept::{Places, cost, place};
use super::*;
use alt_git_codec::{HashAlgo, ObjectKind, RawObject};
use std::collections::HashMap;

/// A tree of `files` files of its own, each under the name every
/// such tree has it under.
fn tree(held: &mut HashMap<ObjectId, RawObject>, of: usize, files: usize) -> ObjectId {
    let mut data = Vec::new();
    for n in 0..files {
        let blob = format!("the file {n} of the tree {of}");
        let blob = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, blob.as_bytes());
        data.extend_from_slice(format!("100644 file-{n:03}\0").as_bytes());
        data.extend_from_slice(blob.as_bytes());
    }
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &data);
    let kind = ObjectKind::Tree;
    held.insert(oid, RawObject { kind, data });
    oid
}

fn trees_of(source: &Source<'_>, oid: &ObjectId) -> Places {
    let tree = entries(source, oid).unwrap().unwrap();
    tree.entries.iter().map(place).collect()
}

/// A tree of the push in the place of a hundred trees of the history
/// served, with a file that none of them has: every one of them is
/// read, and no more of them are kept than fit.
#[test]
fn the_trees_kept_of_the_history_served_fit_their_limit() {
    let mut held = HashMap::new();
    let bases: Vec<ObjectId> = (0..100).map(|n| tree(&mut held, n, 50)).collect();
    let root = tree(&mut held, 1000, 50);
    let reads = std::cell::Cell::new(0);
    let get = |oid: &ObjectId| -> Res<Option<RawObject>> {
        reads.set(reads.get() + 1);
        Ok(held.get(oid).cloned())
    };
    let holds = |_: &ObjectId| -> Res<bool> { Ok(true) };
    let fails = |_: &ObjectId| -> Res<Option<String>> { Ok(None) };
    let source = Source {
        get: &get,
        commit: &|_| Ok(None),
        holds: &holds,
        had: &holds,
        fails: &fails,
    };
    let one = cost(
        &entries(&source, &bases[0])
            .unwrap()
            .unwrap()
            .entries
            .iter()
            .map(place)
            .collect(),
    );
    for (limit, fit) in [(BASES, 100), (10 * one, 10), (one - 1, 0)] {
        let mut trees = Trees::reading(limit, usize::MAX);
        reads.set(0);
        assert!(trees.whole(&source, root, &bases).unwrap());
        assert_eq!(reads.get(), 101, "within {limit}");
        assert_eq!(trees.served.most, fit * one, "within {limit}");
        assert_eq!(trees.added.len(), 51, "within {limit}");
        // looked at again: where trees were let go of, the first of
        // them to come up again takes the place of those still kept
        trees.whole = KeyedIdSet::default();
        assert!(trees.whole(&source, root, &bases).unwrap());
        let again = if fit == 100 { 0 } else { 100 };
        assert_eq!(reads.get(), 101 + 1 + again, "within {limit}");
    }
}

/// Ten trees fit. The one let go of for an eleventh is the one not
/// looked at for the longest, whichever was read first.
#[test]
fn the_tree_let_go_of_is_the_one_looked_at_longest_ago() {
    let mut held = HashMap::new();
    let bases: Vec<ObjectId> = (0..12).map(|n| tree(&mut held, n, 50)).collect();
    let reads = std::cell::Cell::new(0);
    let get = |oid: &ObjectId| -> Res<Option<RawObject>> {
        reads.set(reads.get() + 1);
        Ok(held.get(oid).cloned())
    };
    let source = Source {
        get: &get,
        commit: &|_| Ok(None),
        holds: &|_| Ok(true),
        had: &|_| Ok(true),
        fails: &|_| Ok(None),
    };
    let one = cost(&trees_of(&source, &bases[0]));
    reads.set(0);
    let mut trees = Trees::reading(10 * one, usize::MAX);
    for base in &bases[..10] {
        trees.served.of(&source, base).unwrap().unwrap();
    }
    // the first is looked at again, many times over
    for _ in 0..1000 {
        trees.served.of(&source, &bases[0]).unwrap().unwrap();
    }
    assert_eq!(reads.get(), 10);
    assert!(
        trees.served.looked.len() < 100,
        "{}",
        trees.served.looked.len()
    );
    // the eleventh takes the place of the second, the twelfth of the third
    for (new, kept, gone) in [(10, 0, 1), (11, 0, 2)] {
        trees.served.of(&source, &bases[new]).unwrap().unwrap();
        let before = reads.get();
        trees.served.of(&source, &bases[kept]).unwrap().unwrap();
        assert_eq!(reads.get(), before, "{new}");
        assert!(!trees.served.bases.contains_key(&bases[gone]), "{new}");
        assert_eq!(trees.served.kept, 10 * one);
    }
}

/// A push that has the same trees read again and again is refused
/// once what was read for it is more than it may have read, and
/// nothing more is read for it then.
#[test]
fn a_push_that_has_too_much_read_of_the_history_served_is_refused() {
    let mut held = HashMap::new();
    let bases: Vec<ObjectId> = (0..100).map(|n| tree(&mut held, n, 50)).collect();
    let root = tree(&mut held, 1000, 50);
    let reads = std::cell::Cell::new(0);
    let get = |oid: &ObjectId| -> Res<Option<RawObject>> {
        reads.set(reads.get() + 1);
        Ok(held.get(oid).cloned())
    };
    let source = Source {
        get: &get,
        commit: &|_| Ok(None),
        holds: &|_| Ok(true),
        had: &|_| Ok(true),
        fails: &|_| Ok(None),
    };
    let one = cost(&trees_of(&source, &bases[0]));
    reads.set(0);
    // room for ten of the hundred, and for all of them read twice and a half
    let mut trees = Trees::reading(10 * one, 250 * one);
    for pass in 0..2 {
        trees.whole = KeyedIdSet::default();
        assert!(trees.whole(&source, root, &bases).unwrap(), "{pass}");
        assert_eq!(trees.lack(), super::super::MISSING);
    }
    trees.whole = KeyedIdSet::default();
    assert!(!trees.whole(&source, root, &bases).unwrap());
    assert_eq!(trees.lack(), COSTLY);
    // the root three times, the hundred twice and fifty-one of them once more
    assert_eq!(reads.get(), 3 + 200 + 51);
    trees.whole = KeyedIdSet::default();
    assert!(!trees.whole(&source, root, &bases).unwrap());
    assert_eq!(reads.get(), 3 + 200 + 51 + 1);
}

/// A tree of the history served with `dirs` directories, each a tree of
/// its own, and the tree of the push that has a file more in each of them.
fn wide(held: &mut HashMap<ObjectId, RawObject>, of: usize, dirs: usize) -> ObjectId {
    let mut data = Vec::new();
    for n in 0..dirs {
        let under = tree(held, of * 1000 + n, 1);
        data.extend_from_slice(format!("40000 dir-{n:03}\0").as_bytes());
        data.extend_from_slice(under.as_bytes());
    }
    let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, &data);
    let kind = ObjectKind::Tree;
    held.insert(oid, RawObject { kind, data });
    oid
}

/// A merge of forty commits whose trees do not fit together, with thirty
/// directories of its own. Each of the forty trees is read once for the
/// thirty directories, not once for each of them; and once for each where
/// no more than the trees in the place of one directory may be held.
#[test]
fn a_tree_of_the_history_served_is_read_once_for_all_the_directories_of_a_tree_in_its_place() {
    let mut held = HashMap::new();
    let bases: Vec<ObjectId> = (0..40).map(|n| wide(&mut held, n, 30)).collect();
    let root = wide(&mut held, 77, 30);
    let reads = std::cell::RefCell::new(HashMap::<ObjectId, usize>::new());
    let get = |oid: &ObjectId| -> Res<Option<RawObject>> {
        *reads.borrow_mut().entry(*oid).or_default() += 1;
        Ok(held.get(oid).cloned())
    };
    let source = Source {
        get: &get,
        commit: &|_| Ok(None),
        holds: &|_| Ok(true),
        had: &|_| Ok(true),
        fails: &|_| Ok(None),
    };
    let one = cost(&trees_of(&source, &bases[0]));
    let most = |reads: &HashMap<ObjectId, usize>| bases.iter().map(|base| reads[base]).max();
    for (places, each) in [(PLACES, 1), (40, 30)] {
        reads.borrow_mut().clear();
        // room for three of the forty
        let mut trees = Trees::reading(3 * one, usize::MAX);
        trees.places = places;
        assert!(trees.whole(&source, root, &bases).unwrap());
        assert_eq!(most(&reads.borrow()), Some(each), "holding {places}");
        // the root, its thirty directories and the file of each
        assert_eq!(trees.added.len(), 61, "holding {places}");
    }
}
