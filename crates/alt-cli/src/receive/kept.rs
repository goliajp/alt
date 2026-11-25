//! Commits read while pushes are looked at, kept from one push to the
//! next.
//!
//! A push beside the refs has every commit a ref points at read, to
//! tell which of the commits under it the refs reach: with ten thousand
//! refs that is ten thousand objects taken out of the store and parsed
//! for each such push, the same ones every time. A commit is what its
//! id says, in whichever repository: what the walk goes by of it is
//! kept by its id, for all the repositories this process serves, within
//! [`LIMIT`].

use std::sync::{Arc, Mutex};

use alt_git_codec::ObjectId;
use alt_hash::KeyedIdMap;

use crate::added_commits::Lineage;

/// The bytes the commits kept may count for, as [`cost`] counts them;
/// all of them are let go of once one more would not fit.
const LIMIT: usize = 64 << 20;

#[derive(Default)]
struct Kept {
    commits: KeyedIdMap<ObjectId, Arc<Lineage>>,
    bytes: usize,
}

static KEPT: Mutex<Option<Kept>> = Mutex::new(None);

/// What a commit kept counts for: about what it takes in memory, its
/// place in the table included.
fn cost(commit: &Lineage) -> usize {
    const EACH: usize = 256;
    let parents = std::mem::size_of_val(&*commit.parents);
    EACH + parents + commit.unreadable.as_ref().map_or(0, String::len)
}

/// The commit `oid` as it was read before.
pub(super) fn commit(oid: &ObjectId) -> Option<Arc<Lineage>> {
    let kept = KEPT.lock().unwrap();
    kept.as_ref()?.commits.get(oid).cloned()
}

/// Keeps what the commit `oid` was read as.
pub(super) fn keep(oid: &ObjectId, commit: &Arc<Lineage>) {
    let cost = cost(commit);
    let mut kept = KEPT.lock().unwrap();
    let kept = kept.get_or_insert_with(Kept::default);
    if kept.bytes + cost > LIMIT {
        *kept = Kept::default();
    }
    if kept.commits.insert(*oid, Arc::clone(commit)).is_none() {
        kept.bytes += cost;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use alt_git_codec::{HashAlgo, ObjectId, ObjectKind, RawObject};
    use alt_odb::NativeOdb;

    use crate::added_commits::Lineage;

    use super::super::Objects;

    fn commit(message: &str) -> (ObjectId, Vec<u8>) {
        let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, b"");
        let who = "t <t@e> 1700000000 +0000";
        let above = format!("tree {tree}\nparent {tree}\nparent {tree}");
        let data = format!("{above}\nauthor {who}\ncommitter {who}\n\n{message}\n");
        let oid = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, data.as_bytes());
        (oid, data.into_bytes())
    }

    /// The messages are of this test alone: the commits kept are those of
    /// the whole process, and another test keeps none of these.
    #[test]
    fn a_commit_kept_is_read_as_it_is_in_the_store_and_is_none_where_the_store_lacks_it() {
        let (here, elsewhere) = (commit("kept: here"), commit("kept: elsewhere"));
        let dirs = [(); 2].map(|()| alt_os::temp::tempdir().unwrap());
        let mut odbs = dirs
            .each_ref()
            .map(|dir| NativeOdb::open(dir.path()).unwrap());
        odbs[0].put(here.0, ObjectKind::Commit, &here.1).unwrap();
        odbs[1]
            .put(elsewhere.0, ObjectKind::Commit, &elsewhere.1)
            .unwrap();
        for odb in &mut odbs {
            odb.flush().unwrap();
        }
        let [first, second] = odbs.each_ref().map(|odb| Objects { odb, pack: None });

        // read out of the store, and then as it was kept: the same commit
        assert!(super::commit(&here.0).is_none());
        let read = first.commit(&here.0).unwrap().unwrap();
        let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, b"");
        assert_eq!((&*read.parents, read.tree), (&[tree; 2][..], Some(tree)));
        assert_eq!((read.date, &read.unreadable), (1_700_000_000, &None));
        assert!(Arc::ptr_eq(&super::commit(&here.0).unwrap(), &read));
        assert!(Arc::ptr_eq(&first.commit(&here.0).unwrap().unwrap(), &read));
        assert_eq!(first.get(&here.0).unwrap().unwrap().data, here.1);

        // kept from one store, it is no commit of a store that lacks it
        assert!(second.commit(&here.0).unwrap().is_none());
        second.commit(&elsewhere.0).unwrap().unwrap();
        assert!(super::commit(&elsewhere.0).is_some());
        assert!(first.commit(&elsewhere.0).unwrap().is_none());

        // and once that store has it, it is read there as anywhere
        odbs[1].put(here.0, ObjectKind::Commit, &here.1).unwrap();
        odbs[1].flush().unwrap();
        let second = Objects {
            odb: &odbs[1],
            pack: None,
        };
        assert!(Arc::ptr_eq(
            &second.commit(&here.0).unwrap().unwrap(),
            &read
        ));
    }

    /// What is kept counts for more than it takes: a commit of two
    /// parents, the table's slot for it and the allocations around it.
    #[test]
    fn what_a_commit_kept_counts_for_covers_what_it_takes() {
        let (oid, data) = commit("kept: counted");
        let kind = ObjectKind::Commit;
        let read = Lineage::of(&oid, Some(&RawObject { kind, data })).unwrap();
        let slot = 2 * std::mem::size_of::<(ObjectId, Arc<Lineage>)>();
        let held = std::mem::size_of::<Lineage>() + std::mem::size_of_val(&*read.parents);
        // two words of counts in an `Arc`, two of bookkeeping for each allocation
        assert!(super::cost(&read) >= slot + held + 6 * std::mem::size_of::<usize>());
    }
}
