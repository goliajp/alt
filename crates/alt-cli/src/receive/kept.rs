//! Commits read while pushes are looked at, kept from one push to the
//! next.
//!
//! A push beside the refs has every commit a ref points at read, to
//! tell which of the commits under it the refs reach: with ten thousand
//! refs that is ten thousand objects taken out of the store for each
//! such push, the same ones every time. A commit is what its id says,
//! in whichever repository: its bytes are kept by its id, for all the
//! repositories this process serves, within [`LIMIT`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alt_git_codec::ObjectId;

/// The bytes of commits kept; all of them are let go of once one more
/// would not fit.
const LIMIT: usize = 32 << 20;
/// A commit larger than this is not kept.
const LARGEST: usize = 64 << 10;

#[derive(Default)]
struct Kept {
    commits: HashMap<ObjectId, Arc<Vec<u8>>>,
    bytes: usize,
}

static KEPT: Mutex<Option<Kept>> = Mutex::new(None);

/// The commit `oid` as it was read before.
pub(super) fn commit(oid: &ObjectId) -> Option<Arc<Vec<u8>>> {
    let kept = KEPT.lock().unwrap();
    kept.as_ref()?.commits.get(oid).cloned()
}

/// Keeps `data`, which the commit `oid` was read as.
pub(super) fn keep(oid: &ObjectId, data: &[u8]) {
    if data.len() > LARGEST {
        return;
    }
    let mut kept = KEPT.lock().unwrap();
    let kept = kept.get_or_insert_with(Kept::default);
    if kept.bytes + data.len() > LIMIT {
        *kept = Kept::default();
    }
    if kept.commits.insert(*oid, Arc::new(data.to_vec())).is_none() {
        kept.bytes += data.len();
    }
}

#[cfg(test)]
mod tests {
    use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
    use alt_odb::NativeOdb;

    use super::super::Objects;

    fn commit(message: &str) -> (ObjectId, Vec<u8>) {
        let tree = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Tree, b"");
        let who = "t <t@e> 1700000000 +0000";
        let data = format!("tree {tree}\nauthor {who}\ncommitter {who}\n\n{message}\n");
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

        // read out of the store, and then as it was kept: the same object
        assert!(super::commit(&here.0).is_none());
        let read = first.get(&here.0).unwrap().unwrap();
        assert_eq!((read.kind, &read.data), (ObjectKind::Commit, &here.1));
        assert_eq!(*super::commit(&here.0).unwrap(), here.1);
        let again = first.get(&here.0).unwrap().unwrap();
        assert_eq!((again.kind, again.data), (read.kind, read.data));

        // kept from one store, it is no object of a store that lacks it
        assert!(second.get(&here.0).unwrap().is_none());
        second.get(&elsewhere.0).unwrap().unwrap();
        assert!(super::commit(&elsewhere.0).is_some());
        assert!(first.get(&elsewhere.0).unwrap().is_none());

        // and once that store has it, it is read there as anywhere
        odbs[1].put(here.0, ObjectKind::Commit, &here.1).unwrap();
        odbs[1].flush().unwrap();
        let second = Objects {
            odb: &odbs[1],
            pack: None,
        };
        assert_eq!(second.get(&here.0).unwrap().unwrap().data, here.1);
    }
}
